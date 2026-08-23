//! Cleanup ordering, exact launch identity, deadlines, and verification policy.

use std::sync::Arc;

use aworkit_protocol::{
    BootstrapDeadlinesV1, ManualRecoveryNoticeV1, focused_verification_evidence_hash_v1,
    focused_verification_plan_hash_v1,
};
use aworkit_protocol::{CapabilityOutcomeClassV1, ProcessGeneration, StableId};

use crate::journal::canonical_hash;
use crate::protocol::MAX_BOOTSTRAP_DEADLINE_MS;
use crate::slots::BuildSlotVerifyPortV1;

use super::model::*;
use super::ports::{ApplicationLaunchWatchdogPortV1, PlatformProcessPortV1};

/// Platform-neutral activation generation launcher and watchdog.
pub struct ApplicationLaunchWatchdog {
    process: Arc<dyn PlatformProcessPortV1>,
    slots: Arc<dyn BuildSlotVerifyPortV1>,
    helper_protocol_version: u16,
}

impl ApplicationLaunchWatchdog {
    #[must_use]
    pub fn new(
        process: Arc<dyn PlatformProcessPortV1>,
        slots: Arc<dyn BuildSlotVerifyPortV1>,
        helper_protocol_version: u16,
    ) -> Self {
        Self {
            process,
            slots,
            helper_protocol_version,
        }
    }

    fn failure(
        spec: &GenerationLaunchSpecV1,
        stage: WatchdogFailureStageV1,
        reason_code: &str,
    ) -> WatchdogFailureV1 {
        let digest = canonical_hash(&(
            &spec.activation_id,
            &spec.attempt_id,
            spec.role,
            stage,
            reason_code,
        ))
        .unwrap_or_else(|_| {
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_owned()
        });
        let diagnostic_id = StableId::parse(format!("watchdog.{}", &digest[7..39]))
            .unwrap_or_else(|_| StableId::parse("watchdog.invalid").expect("static id"));
        WatchdogFailureV1 {
            activation_id: spec.activation_id.clone(),
            attempt_id: spec.attempt_id.clone(),
            role: spec.role,
            stage,
            reason_code: reason_code.to_owned(),
            diagnostic_id,
            rollback_required: true,
        }
    }

    fn cleanup_failure(
        activation_id: &StableId,
        attempt_id: &StableId,
        reason_code: &str,
        rollback_required: bool,
    ) -> WatchdogFailureV1 {
        let spec = GenerationLaunchSpecV1 {
            activation_id: activation_id.clone(),
            attempt_id: attempt_id.clone(),
            role: GenerationRoleV1::Candidate,
            installation_id: StableId::parse("invalid.installation").expect("static id"),
            enrollment_digest: String::new(),
            capability_generation: 0,
            capability_digest: String::new(),
            verification_plan_hash: String::new(),
            verification_plan: aworkit_protocol::FocusedVerificationPlanV1 {
                plan_id: StableId::parse("invalid.plan").expect("static id"),
                checks: Vec::new(),
                plan_hash: String::new(),
            },
            process_generation: ProcessGeneration(0),
            expected_prior_process_generation: ProcessGeneration(0),
            slot: invalid_slot(),
            selector: invalid_selector(),
            prior_cleanup: invalid_cleanup(),
            helper_detached_and_surviving: false,
            deadlines: invalid_deadlines(),
        };
        let mut failure = Self::failure(&spec, WatchdogFailureStageV1::Cleanup, reason_code);
        failure.rollback_required = rollback_required;
        failure
    }

    fn validate_deadlines(deadlines: &BootstrapDeadlinesV1) -> bool {
        [
            deadlines.admission_ms,
            deadlines.cleanup_ms,
            deadlines.startup_ms,
            deadlines.focused_verification_ms,
            deadlines.rollback_ms,
            deadlines.result_read_ms,
        ]
        .into_iter()
        .all(|value| value > 0 && value <= MAX_BOOTSTRAP_DEADLINE_MS)
    }

    fn verify_cleanup(proof: &ProcessTreeCleanupV1) -> bool {
        let mut unhashed = proof.clone();
        unhashed.proof_hash.clear();
        proof.tree_empty
            && !proof.orphan_risk
            && canonical_hash(&unhashed).is_ok_and(|hash| hash == proof.proof_hash)
    }

    fn validate_preconditions(
        &self,
        spec: &GenerationLaunchSpecV1,
    ) -> Result<(), WatchdogFailureV1> {
        if !spec.helper_detached_and_surviving
            || spec.process_generation.0 == 0
            || spec.expected_prior_process_generation.0 == 0
            || spec.process_generation == spec.expected_prior_process_generation
            || spec.prior_cleanup.process_generation != spec.expected_prior_process_generation
            || !Self::verify_cleanup(&spec.prior_cleanup)
            || !Self::validate_deadlines(&spec.deadlines)
            || spec.capability_generation == 0
            || spec.enrollment_digest.is_empty()
            || spec.capability_digest.is_empty()
            || spec.verification_plan_hash.is_empty()
            || spec.verification_plan.checks.is_empty()
            || !focused_verification_plan_hash_v1(&spec.verification_plan)
                .is_ok_and(|hash| hash == spec.verification_plan_hash)
        {
            return Err(Self::failure(
                spec,
                WatchdogFailureStageV1::Preconditions,
                "launch_precondition_missing",
            ));
        }
        let slot = self
            .slots
            .reverify_opened_slot(&spec.slot.handle)
            .map_err(|_| {
                Self::failure(
                    spec,
                    WatchdogFailureStageV1::Preconditions,
                    "slot_identity_changed",
                )
            })?;
        if slot != spec.slot
            || spec.selector.selected_build_content_hash != spec.slot.build_content_hash
            || spec.selector.selected_root_identity_hash != spec.slot.root_identity_hash
            || spec.selector.capability_generation != spec.capability_generation
        {
            return Err(Self::failure(
                spec,
                WatchdogFailureStageV1::Preconditions,
                "selector_or_slot_mismatch",
            ));
        }
        Ok(())
    }

    fn hash_is_valid<T: serde::Serialize + Clone>(
        value: &T,
        clear: impl FnOnce(&mut T),
        expected: &str,
    ) -> bool {
        let mut unhashed = value.clone();
        clear(&mut unhashed);
        canonical_hash(&unhashed).is_ok_and(|hash| hash == expected)
    }

    fn validate_launch(
        spec: &GenerationLaunchSpecV1,
        request: &PlatformLaunchRequestV1,
        launch: &LaunchObservationV1,
    ) -> bool {
        launch.attempt_id == spec.attempt_id
            && launch.process_tree.process_generation == spec.process_generation
            && launch.executable_hash == spec.slot.build_content_hash
            && launch.slot_root_identity_hash == spec.slot.root_identity_hash
            && launch.process_tree.process_generation == request.process_generation
            && Self::hash_is_valid(
                launch,
                |value| value.observation_hash.clear(),
                &launch.observation_hash,
            )
    }

    fn validate_handshake(
        &self,
        spec: &GenerationLaunchSpecV1,
        request: &PlatformLaunchRequestV1,
        handshake: &GenerationHandshakeV1,
    ) -> bool {
        handshake.activation_id == spec.activation_id
            && handshake.attempt_id == spec.attempt_id
            && handshake.installation_id == spec.installation_id
            && handshake.enrollment_digest == spec.enrollment_digest
            && handshake.capability_generation == spec.capability_generation
            && handshake.capability_digest == spec.capability_digest
            && handshake.launch_nonce_hash == request.launch_nonce_hash
            && handshake.executable_hash == spec.slot.build_content_hash
            && handshake.slot_root_identity_hash == spec.slot.root_identity_hash
            && handshake.helper_protocol_version == self.helper_protocol_version
            && handshake.verification_plan_hash == spec.verification_plan_hash
            && handshake.mode == BootstrapLaunchModeV1::VerificationOnly
            && handshake.process_generation == spec.process_generation
            && Self::hash_is_valid(
                handshake,
                |value| value.handshake_hash.clear(),
                &handshake.handshake_hash,
            )
    }

    fn validate_health(spec: &GenerationLaunchSpecV1, health: &GenerationHealthV1) -> bool {
        health.attempt_id == spec.attempt_id
            && health.process_generation == spec.process_generation
            && health.healthy
            && Self::hash_is_valid(
                health,
                |value| value.observation_hash.clear(),
                &health.observation_hash,
            )
    }

    fn validate_verification(
        spec: &GenerationLaunchSpecV1,
        verification: &FocusedVerificationResultV1,
    ) -> bool {
        verification.activation_id == spec.activation_id
            && verification.attempt_id == spec.attempt_id
            && verification.process_generation == spec.process_generation
            && verification.verification_plan_hash == spec.verification_plan_hash
            && verification.passed
            && verification.outcome.class == CapabilityOutcomeClassV1::Success
            && verification.focused_verification.plan_id == spec.verification_plan.plan_id
            && verification.focused_verification.plan_hash == spec.verification_plan_hash
            && verification.focused_verification.results.len()
                == spec.verification_plan.checks.len()
            && spec.verification_plan.checks.iter().all(|check| {
                verification
                    .focused_verification
                    .results
                    .iter()
                    .any(|result| result.check_id == check.check_id && result.passed)
            })
            && focused_verification_evidence_hash_v1(&verification.focused_verification)
                .is_ok_and(|hash| hash == verification.focused_verification.evidence_hash)
            && Self::hash_is_valid(
                verification,
                |value| value.result_hash.clear(),
                &verification.result_hash,
            )
    }
}

impl ApplicationLaunchWatchdogPortV1 for ApplicationLaunchWatchdog {
    fn cleanup_generation(
        &self,
        activation_id: &StableId,
        attempt_id: &StableId,
        generation: ProcessGeneration,
        deadlines: &BootstrapDeadlinesV1,
        rollback_required: bool,
    ) -> Result<ProcessTreeCleanupV1, WatchdogFailureV1> {
        if generation.0 == 0 || !Self::validate_deadlines(deadlines) {
            return Err(Self::cleanup_failure(
                activation_id,
                attempt_id,
                "cleanup_precondition_missing",
                rollback_required,
            ));
        }
        self.process
            .request_cooperative_shutdown(generation)
            .map_err(|_| {
                Self::cleanup_failure(
                    activation_id,
                    attempt_id,
                    "cooperative_shutdown_failed",
                    rollback_required,
                )
            })?;
        let exited = self
            .process
            .await_tree_exit(generation, deadlines.cleanup_ms)
            .map_err(|_| {
                Self::cleanup_failure(
                    activation_id,
                    attempt_id,
                    "cleanup_wait_failed",
                    rollback_required,
                )
            })?;
        if !exited {
            self.process
                .force_terminate_tree(generation, deadlines.cleanup_ms)
                .map_err(|_| {
                    Self::cleanup_failure(
                        activation_id,
                        attempt_id,
                        "forced_cleanup_failed",
                        rollback_required,
                    )
                })?;
        }
        let proof = self.process.prove_tree_empty(generation).map_err(|_| {
            Self::cleanup_failure(
                activation_id,
                attempt_id,
                "cleanup_proof_missing",
                rollback_required,
            )
        })?;
        if proof.process_generation != generation || !Self::verify_cleanup(&proof) {
            return Err(Self::cleanup_failure(
                activation_id,
                attempt_id,
                "cleanup_proof_ambiguous",
                rollback_required,
            ));
        }
        Ok(proof)
    }

    fn launch_and_watch(
        &self,
        spec: &GenerationLaunchSpecV1,
    ) -> Result<GenerationWatchdogSuccessV1, WatchdogFailureV1> {
        self.validate_preconditions(spec)?;
        let outcome = (|| {
            let mut nonce = [0_u8; 32];
            getrandom::fill(&mut nonce).map_err(|_| {
                Self::failure(
                    spec,
                    WatchdogFailureStageV1::Spawn,
                    "launch_nonce_unavailable",
                )
            })?;
            let launch_nonce_hash = canonical_hash(&nonce).map_err(|_| {
                Self::failure(
                    spec,
                    WatchdogFailureStageV1::Spawn,
                    "launch_nonce_unavailable",
                )
            })?;
            let request = PlatformLaunchRequestV1 {
                activation_id: spec.activation_id.clone(),
                attempt_id: spec.attempt_id.clone(),
                installation_id: spec.installation_id.clone(),
                enrollment_digest: spec.enrollment_digest.clone(),
                capability_generation: spec.capability_generation,
                capability_digest: spec.capability_digest.clone(),
                verification_plan_hash: spec.verification_plan_hash.clone(),
                verification_plan_id: spec.verification_plan.plan_id.clone(),
                verification_check_ids: spec
                    .verification_plan
                    .checks
                    .iter()
                    .map(|check| check.check_id.clone())
                    .collect(),
                helper_protocol_version: self.helper_protocol_version,
                role: spec.role,
                mode: BootstrapLaunchModeV1::VerificationOnly,
                process_generation: spec.process_generation,
                slot_handle: spec.slot.handle.clone(),
                exact_core_entry: spec.slot.expected_core_entry.clone(),
                launch_nonce_hash,
                sanitized_environment: true,
                inherited_handles_closed: true,
            };
            let launch = self
                .process
                .spawn_verified(&request)
                .map_err(|_| Self::failure(spec, WatchdogFailureStageV1::Spawn, "spawn_failed"))?;
            if !Self::validate_launch(spec, &request, &launch) {
                return Err(Self::failure(
                    spec,
                    WatchdogFailureStageV1::Identity,
                    "spawn_identity_mismatch",
                ));
            }
            let handshake = self
                .process
                .await_identity_handshake(&launch.process_tree, spec.deadlines.startup_ms)
                .map_err(|_| {
                    Self::failure(spec, WatchdogFailureStageV1::Startup, "startup_wait_failed")
                })?
                .ok_or_else(|| {
                    Self::failure(
                        spec,
                        WatchdogFailureStageV1::Startup,
                        "startup_deadline_expired",
                    )
                })?;
            if !self.validate_handshake(spec, &request, &handshake) {
                return Err(Self::failure(
                    spec,
                    WatchdogFailureStageV1::Identity,
                    "generation_handshake_mismatch",
                ));
            }
            let health = self
                .process
                .health_snapshot(&launch.process_tree, spec.deadlines.startup_ms)
                .map_err(|_| {
                    Self::failure(spec, WatchdogFailureStageV1::Health, "health_wait_failed")
                })?
                .ok_or_else(|| {
                    Self::failure(
                        spec,
                        WatchdogFailureStageV1::Health,
                        "health_deadline_expired",
                    )
                })?;
            if !Self::validate_health(spec, &health) {
                return Err(Self::failure(
                    spec,
                    WatchdogFailureStageV1::Health,
                    "generation_unhealthy",
                ));
            }
            if spec.role == GenerationRoleV1::Previous {
                return Ok(GenerationWatchdogSuccessV1::PreviousHealthy {
                    launch,
                    handshake,
                    health,
                });
            }
            self.process
                .handoff_focused_verification(&launch.process_tree, &spec.verification_plan_hash)
                .map_err(|_| {
                    Self::failure(
                        spec,
                        WatchdogFailureStageV1::FocusedVerification,
                        "verification_handoff_failed",
                    )
                })?;
            let verification = self
                .process
                .await_focused_verification(
                    &launch.process_tree,
                    spec.deadlines.focused_verification_ms,
                )
                .map_err(|_| {
                    Self::failure(
                        spec,
                        WatchdogFailureStageV1::FocusedVerification,
                        "verification_wait_failed",
                    )
                })?
                .ok_or_else(|| {
                    Self::failure(
                        spec,
                        WatchdogFailureStageV1::FocusedVerification,
                        "verification_deadline_expired",
                    )
                })?;
            if !Self::validate_verification(spec, &verification) {
                return Err(Self::failure(
                    spec,
                    WatchdogFailureStageV1::FocusedVerification,
                    "focused_verification_failed",
                ));
            }
            Ok(GenerationWatchdogSuccessV1::CandidateVerified {
                launch,
                handshake,
                health,
                verification,
            })
        })();
        if let Err(failure) = &outcome {
            if matches!(
                failure.stage,
                WatchdogFailureStageV1::Startup
                    | WatchdogFailureStageV1::Identity
                    | WatchdogFailureStageV1::Health
                    | WatchdogFailureStageV1::FocusedVerification
            ) {
                self.cleanup_generation(
                    &spec.activation_id,
                    &spec.attempt_id,
                    spec.process_generation,
                    &spec.deadlines,
                    true,
                )?;
            }
        }
        outcome
    }

    fn stable_launcher_notice(&self, notice: &ManualRecoveryNoticeV1) -> StableLauncherNoticeV1 {
        StableLauncherNoticeV1 {
            notice: notice.clone(),
            copy_diagnostic_id_allowed: true,
            open_recovery_instructions_allowed: true,
            exits_after_notice: true,
        }
    }
}

fn invalid_slot() -> crate::slots::VerifiedBuildSlotV1 {
    crate::slots::VerifiedBuildSlotV1 {
        build_content_hash: String::new(),
        manifest_hash: String::new(),
        root_identity_hash: String::new(),
        owner_identity_hash: String::new(),
        volume_identity_hash: String::new(),
        expected_core_entry: String::new(),
        data_compatibility: crate::slots::SlotDataCompatibilityV1::RollbackCompatible,
        handle: crate::slots::OpenBuildSlotHandleV1 {
            handle_id: StableId::parse("invalid.handle").expect("static id"),
            build_content_hash: String::new(),
            root_identity_hash: String::new(),
            manifest_hash: String::new(),
            verification_generation: 0,
        },
    }
}

fn invalid_selector() -> crate::profile::ActiveSelectorObservationV1 {
    crate::profile::ActiveSelectorObservationV1 {
        selector_identity_hash: String::new(),
        selected_build_content_hash: String::new(),
        selected_root_identity_hash: String::new(),
        capability_generation: 0,
        observation_hash: String::new(),
    }
}

fn invalid_cleanup() -> ProcessTreeCleanupV1 {
    ProcessTreeCleanupV1 {
        process_generation: ProcessGeneration(0),
        cooperative_requested: false,
        forced_termination_used: false,
        descendants_observed: 0,
        tree_empty: false,
        orphan_risk: true,
        proof_hash: String::new(),
    }
}

fn invalid_deadlines() -> BootstrapDeadlinesV1 {
    BootstrapDeadlinesV1 {
        admission_ms: 0,
        cleanup_ms: 0,
        startup_ms: 0,
        focused_verification_ms: 0,
        rollback_ms: 0,
        result_read_ms: 0,
    }
}
