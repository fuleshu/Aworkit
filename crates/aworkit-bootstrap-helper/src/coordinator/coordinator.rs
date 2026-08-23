//! Journal-first activation and deterministic rollback orchestration.

use std::sync::{Arc, Mutex, TryLockError};

use aworkit_protocol::{
    BootstrapResultKindV1, BootstrapResultV1, BuildProvenanceV1, ManualRecoveryNoticeV1,
    PlatformReasonV1, ReasonCodeV1, RepairArtifactRefV1, bootstrap_result_hash_v1,
    focused_verification_plan_hash_v1,
};
use aworkit_protocol::{ProcessGeneration, StableId};

use crate::journal::{
    ActivationJournalPortV1, BatonAcceptedV1, BootstrapEffectV1, BootstrapJournalMutationV1,
    BootstrapPhaseAdvanceV1, BootstrapPhaseV1, canonical_hash,
};
use crate::profile::{ActivationPlanV1, PlatformActivationPortV1};
use crate::protocol::{BootstrapEnrollmentPortV1, EnrollmentPlanV1, EnrollmentPreparationV1};
use crate::slots::{BuildSlotVerifyPortV1, VerifiedBuildSlotV1};
use crate::watchdog::{
    ApplicationLaunchWatchdogPortV1, GenerationLaunchSpecV1, GenerationRoleV1,
    GenerationWatchdogSuccessV1, ProcessTreeCleanupV1, WatchdogFailureV1,
};

use super::error::CoordinatorError;
use super::model::ActivationExecutionV1;
use super::ports::ActivationControlPortV1;

/// Single writer of activation effects and phase changes.
pub struct ActivationRollbackCoordinator {
    journal: Arc<dyn ActivationJournalPortV1>,
    slots: Arc<dyn BuildSlotVerifyPortV1>,
    selector: Arc<dyn PlatformActivationPortV1>,
    watchdog: Arc<dyn ApplicationLaunchWatchdogPortV1>,
    flight: Mutex<()>,
}

impl ActivationRollbackCoordinator {
    #[must_use]
    pub fn new(
        journal: Arc<dyn ActivationJournalPortV1>,
        slots: Arc<dyn BuildSlotVerifyPortV1>,
        selector: Arc<dyn PlatformActivationPortV1>,
        watchdog: Arc<dyn ApplicationLaunchWatchdogPortV1>,
    ) -> Self {
        Self {
            journal,
            slots,
            selector,
            watchdog,
            flight: Mutex::new(()),
        }
    }

    fn state(
        &self,
        activation_id: &StableId,
    ) -> Result<crate::journal::BootstrapRecoveryStateV1, CoordinatorError> {
        self.journal
            .load_activation_recovery(activation_id)
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?
            .ok_or(CoordinatorError::MissingJournal)
    }

    fn advance(
        &self,
        activation_id: &StableId,
        next_phase: BootstrapPhaseV1,
    ) -> Result<(), CoordinatorError> {
        let state = self.state(activation_id)?;
        self.journal
            .advance_phase(&BootstrapPhaseAdvanceV1 {
                activation_id: activation_id.clone(),
                expected_ordinal: state.head_ordinal + 1,
                expected_phase: state.phase,
                next_phase,
            })
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        Ok(())
    }

    fn intent(
        &self,
        activation_id: &StableId,
        effect: &BootstrapEffectV1,
    ) -> Result<(), CoordinatorError> {
        let state = self.state(activation_id)?;
        self.journal
            .append_effect_intent(&BootstrapJournalMutationV1 {
                activation_id: activation_id.clone(),
                expected_ordinal: state.head_ordinal + 1,
                expected_phase: state.phase,
                effect: effect.clone(),
            })
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        Ok(())
    }

    fn observe(
        &self,
        activation_id: &StableId,
        mut effect: BootstrapEffectV1,
        observation: &impl serde::Serialize,
    ) -> Result<(), CoordinatorError> {
        effect.observation_hash = canonical_hash(observation)
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        let state = self.state(activation_id)?;
        self.journal
            .append_observed_effect(&BootstrapJournalMutationV1 {
                activation_id: activation_id.clone(),
                expected_ordinal: state.head_ordinal + 1,
                expected_phase: state.phase,
                effect,
            })
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        Ok(())
    }

    fn effect(
        current: &VerifiedBuildSlotV1,
        target: &VerifiedBuildSlotV1,
        capability_generation: u64,
        process_generation: ProcessGeneration,
    ) -> BootstrapEffectV1 {
        BootstrapEffectV1 {
            current_slot_hash: current.build_content_hash.clone(),
            target_slot_hash: target.build_content_hash.clone(),
            capability_generation,
            process_generation,
            observation_hash: String::new(),
        }
    }

    fn stable_id(activation_id: &StableId, label: &str) -> Result<StableId, CoordinatorError> {
        let hash = canonical_hash(&(activation_id, label))
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        StableId::parse(format!("bootstrap.{}.{}", label, &hash[7..31]))
            .map_err(|_| CoordinatorError::Fence("derived stable id"))
    }

    fn validate_execution(
        &self,
        execution: &ActivationExecutionV1,
        baton: &BatonAcceptedV1,
    ) -> Result<(), CoordinatorError> {
        let plan = &execution.plan;
        if plan.activation_id != baton.activation_id
            || plan.capability_generation != baton.capability_generation
            || plan.capability_digest != baton.capability_digest
            || plan.candidate.build_content_hash != baton.candidate_slot_hash
            || plan.previous.build_content_hash != baton.previous_slot_hash
            || plan.current_process_generation != baton.current_process_generation
            || plan.candidate_process_generation != baton.candidate_process_generation
            || plan.rollback_process_generation != baton.rollback_process_generation
            || plan.deadlines != baton.deadlines
            || execution.management_checkpoint_id != baton.management_checkpoint_id
            || execution.verification_plan.plan_hash != baton.verification_plan_hash
            || !focused_verification_plan_hash_v1(&execution.verification_plan)
                .is_ok_and(|hash| hash == baton.verification_plan_hash)
            || !execution.helper_detached_and_surviving
        {
            return Err(CoordinatorError::Fence(
                "execution differs from durable baton",
            ));
        }
        Ok(())
    }

    fn reverify_plan(&self, plan: &ActivationPlanV1) -> Result<(), CoordinatorError> {
        for slot in [&plan.current, &plan.candidate, &plan.previous] {
            let observed = self
                .slots
                .reverify_opened_slot(&slot.handle)
                .map_err(|error| CoordinatorError::Slot(error.to_string()))?;
            if &observed != slot {
                return Err(CoordinatorError::Fence("slot identity drift"));
            }
        }
        self.selector
            .verify_selector(plan, &plan.current.handle)
            .map_err(|error| CoordinatorError::Selector(error.to_string()))?;
        Ok(())
    }

    fn launch_spec(
        execution: &ActivationExecutionV1,
        baton: &BatonAcceptedV1,
        role: GenerationRoleV1,
        slot: VerifiedBuildSlotV1,
        selector: crate::profile::ActiveSelectorObservationV1,
        cleanup: ProcessTreeCleanupV1,
        attempt_id: StableId,
    ) -> GenerationLaunchSpecV1 {
        let (process_generation, expected_prior_process_generation) = match role {
            GenerationRoleV1::Candidate => (
                baton.candidate_process_generation,
                baton.current_process_generation,
            ),
            GenerationRoleV1::Previous => (
                baton.rollback_process_generation,
                baton.candidate_process_generation,
            ),
        };
        GenerationLaunchSpecV1 {
            activation_id: baton.activation_id.clone(),
            attempt_id,
            role,
            installation_id: execution.installation_id.clone(),
            enrollment_digest: baton.enrollment_digest.clone(),
            capability_generation: baton.capability_generation,
            capability_digest: baton.capability_digest.clone(),
            verification_plan_hash: baton.verification_plan_hash.clone(),
            verification_plan: execution.verification_plan.clone(),
            process_generation,
            expected_prior_process_generation,
            slot,
            selector,
            prior_cleanup: cleanup,
            helper_detached_and_surviving: execution.helper_detached_and_surviving,
            deadlines: baton.deadlines.clone(),
        }
    }

    fn receipt(
        execution: &ActivationExecutionV1,
        baton: &BatonAcceptedV1,
        recipient: ProcessGeneration,
        result: BootstrapResultKindV1,
        label: &str,
    ) -> Result<BootstrapResultV1, CoordinatorError> {
        let mut receipt = BootstrapResultV1 {
            schema_version: 1,
            receipt_id: Self::stable_id(&baton.activation_id, label)?,
            activation_id: baton.activation_id.clone(),
            baton_hash: baton.baton_hash.clone(),
            management_checkpoint_id: execution.management_checkpoint_id.clone(),
            recipient_process_generation: recipient,
            sealed_at_epoch_ms: execution.sealed_at_epoch_ms,
            result,
            receipt_hash: String::new(),
        };
        receipt.receipt_hash = bootstrap_result_hash_v1(&receipt)
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        Ok(receipt)
    }

    fn seal(&self, receipt: BootstrapResultV1) -> Result<BootstrapResultV1, CoordinatorError> {
        self.journal
            .store_bootstrap_result(&receipt)
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        self.journal
            .seal_terminal()
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        Ok(receipt)
    }

    fn unsupported(
        &self,
        execution: &ActivationExecutionV1,
        baton: &BatonAcceptedV1,
        aborted: bool,
        code: &str,
    ) -> Result<BootstrapResultV1, CoordinatorError> {
        self.advance(
            &baton.activation_id,
            if aborted {
                BootstrapPhaseV1::AbortedBeforeSwitch
            } else {
                BootstrapPhaseV1::Unsupported
            },
        )?;
        let receipt = Self::receipt(
            execution,
            baton,
            baton.current_process_generation,
            BootstrapResultKindV1::Unsupported {
                reason: PlatformReasonV1 {
                    code: code.to_owned(),
                    message: "Activation stopped before the selector changed.".to_owned(),
                    next_steps: vec!["Continue using the current verified build.".to_owned()],
                },
            },
            "unsupported",
        )?;
        self.seal(receipt)
    }

    fn manual(
        &self,
        execution: &ActivationExecutionV1,
        baton: &BatonAcceptedV1,
        reason: ReasonCodeV1,
        observed: &impl serde::Serialize,
    ) -> Result<BootstrapResultV1, CoordinatorError> {
        let state = self.state(&baton.activation_id)?;
        if state.phase != BootstrapPhaseV1::ManualRecoveryRequired {
            self.advance(
                &baton.activation_id,
                BootstrapPhaseV1::ManualRecoveryRequired,
            )?;
        }
        let observed_slot_state_hash = canonical_hash(observed)
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        let diagnostic_id = Self::stable_id(&baton.activation_id, "diagnostic")?;
        let instructions = vec![
            "Exit all application processes.".to_owned(),
            "Start the application only through the stable launcher.".to_owned(),
            "Preserve the diagnostic identifier for support.".to_owned(),
        ];
        let notice = ManualRecoveryNoticeV1 {
            notice_id: Self::stable_id(&baton.activation_id, "notice")?,
            activation_id: baton.activation_id.clone(),
            reason,
            observed_slot_state_hash: observed_slot_state_hash.clone(),
            diagnostic_id: diagnostic_id.clone(),
            instructions: instructions.clone(),
        };
        self.journal
            .store_manual_recovery_notice(&notice)
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        let _stable_notice = self.watchdog.stable_launcher_notice(&notice);
        let receipt = Self::receipt(
            execution,
            baton,
            baton.rollback_process_generation,
            BootstrapResultKindV1::ManualRecoveryRequired {
                diagnostic_id,
                observed_slot_state: observed_slot_state_hash,
                instructions,
            },
            "manual",
        )?;
        self.seal(receipt)
    }

    fn rollback(
        &self,
        execution: &ActivationExecutionV1,
        baton: &BatonAcceptedV1,
        reason: &str,
    ) -> Result<BootstrapResultV1, CoordinatorError> {
        if self.state(&baton.activation_id)?.phase != BootstrapPhaseV1::RollingBack {
            self.advance(&baton.activation_id, BootstrapPhaseV1::RollingBack)?;
        }
        let plan = &execution.plan;
        let cleanup_effect = Self::effect(
            &plan.candidate,
            &plan.previous,
            baton.capability_generation,
            baton.candidate_process_generation,
        );
        self.intent(&baton.activation_id, &cleanup_effect)?;
        let cleanup_attempt = Self::stable_id(&baton.activation_id, "candidate-cleanup")?;
        let cleanup = self.watchdog.cleanup_generation(
            &baton.activation_id,
            &cleanup_attempt,
            baton.candidate_process_generation,
            &baton.deadlines,
            true,
        );
        self.observe(
            &baton.activation_id,
            cleanup_effect,
            &cleanup_observation(&cleanup),
        )?;
        let cleanup = match cleanup {
            Ok(proof) => proof,
            Err(failure) => {
                return self.manual(
                    execution,
                    baton,
                    ReasonCodeV1::GenerationProofMissing,
                    &failure.reason_code,
                );
            }
        };

        let restore_effect = Self::effect(
            &plan.candidate,
            &plan.previous,
            baton.capability_generation,
            baton.rollback_process_generation,
        );
        self.intent(&baton.activation_id, &restore_effect)?;
        let before_restore = self.selector.observe_active_selector(plan);
        let restored = match before_restore {
            Ok(observation)
                if observation.selected_build_content_hash == plan.previous.build_content_hash
                    && observation.selected_root_identity_hash
                        == plan.previous.root_identity_hash =>
            {
                Ok(observation)
            }
            Ok(observation)
                if observation.selected_build_content_hash == plan.candidate.build_content_hash
                    && observation.selected_root_identity_hash
                        == plan.candidate.root_identity_hash =>
            {
                self.selector
                    .restore_previous_selector(plan, BootstrapPhaseV1::RollingBack)
                    .map(|receipt| receipt.after)
            }
            Ok(_) => Err(crate::profile::ProfileError::AmbiguousSelector),
            Err(error) => Err(error),
        };
        self.observe(
            &baton.activation_id,
            restore_effect,
            &selector_state_observation(&restored),
        )?;
        let restored = match restored {
            Ok(observation) => observation,
            Err(error) => {
                return self.manual(
                    execution,
                    baton,
                    ReasonCodeV1::AmbiguousSelectorState,
                    &error.to_string(),
                );
            }
        };
        if let Err(error) = self.slots.reverify_opened_slot(&plan.previous.handle) {
            return self.manual(
                execution,
                baton,
                ReasonCodeV1::OwnershipLost,
                &error.to_string(),
            );
        }
        if let Err(error) = self.selector.verify_selector(plan, &plan.previous.handle) {
            return self.manual(
                execution,
                baton,
                ReasonCodeV1::AmbiguousSelectorState,
                &error.to_string(),
            );
        }
        self.advance(&baton.activation_id, BootstrapPhaseV1::PreviousSelected)?;

        let launch_effect = Self::effect(
            &plan.previous,
            &plan.previous,
            baton.capability_generation,
            baton.rollback_process_generation,
        );
        self.intent(&baton.activation_id, &launch_effect)?;
        let spec = Self::launch_spec(
            execution,
            baton,
            GenerationRoleV1::Previous,
            plan.previous.clone(),
            restored,
            cleanup,
            Self::stable_id(&baton.activation_id, "rollback-launch")?,
        );
        let launch = self.watchdog.launch_and_watch(&spec);
        self.observe(
            &baton.activation_id,
            launch_effect,
            &watchdog_observation(&launch),
        )?;
        self.advance(&baton.activation_id, BootstrapPhaseV1::PreviousRelaunching)?;
        if !matches!(
            launch,
            Ok(GenerationWatchdogSuccessV1::PreviousHealthy { .. })
        ) {
            return self.manual(
                execution,
                baton,
                ReasonCodeV1::RollbackFailure,
                &watchdog_observation(&launch),
            );
        }
        self.advance(&baton.activation_id, BootstrapPhaseV1::RolledBack)?;
        let receipt = Self::receipt(
            execution,
            baton,
            baton.rollback_process_generation,
            BootstrapResultKindV1::RolledBack {
                reason: reason.to_owned(),
                rollback_evidence: vec![Self::rollback_evidence(baton)?],
            },
            "rollback",
        )?;
        self.seal(receipt)
    }

    fn rollback_evidence(baton: &BatonAcceptedV1) -> Result<RepairArtifactRefV1, CoordinatorError> {
        let content_hash = canonical_hash(&(
            &baton.activation_id,
            &baton.candidate_slot_hash,
            &baton.previous_slot_hash,
            baton.rollback_process_generation,
        ))
        .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        Ok(RepairArtifactRefV1 {
            artifact_id: Self::stable_id(&baton.activation_id, "rollback-evidence")?,
            content_hash,
            byte_size: 1,
            media_type: "application/vnd.aworkit.bootstrap-rollback+json".to_owned(),
            logical_name: "bootstrap-rollback-evidence.json".to_owned(),
        })
    }

    fn execute_locked(
        &self,
        execution: &ActivationExecutionV1,
    ) -> Result<BootstrapResultV1, CoordinatorError> {
        let state = self.state(&execution.plan.activation_id)?;
        if let Some(terminal) = state.terminal {
            return Ok(terminal);
        }
        let baton = state.baton.ok_or(CoordinatorError::MissingBaton)?;
        self.validate_execution(execution, &baton)?;
        if state.phase != BootstrapPhaseV1::BatonDurable {
            return Err(CoordinatorError::Fence(
                "activation is not ready to execute",
            ));
        }
        if self.reverify_plan(&execution.plan).is_err() {
            return self.unsupported(execution, &baton, false, "activation_precondition_changed");
        }
        self.advance(&baton.activation_id, BootstrapPhaseV1::SlotsVerified)?;
        self.advance(&baton.activation_id, BootstrapPhaseV1::QuiescingCurrent)?;

        let plan = &execution.plan;
        let cleanup_effect = Self::effect(
            &plan.current,
            &plan.candidate,
            baton.capability_generation,
            baton.current_process_generation,
        );
        self.intent(&baton.activation_id, &cleanup_effect)?;
        let cleanup_attempt = Self::stable_id(&baton.activation_id, "current-cleanup")?;
        let cleanup = self.watchdog.cleanup_generation(
            &baton.activation_id,
            &cleanup_attempt,
            baton.current_process_generation,
            &baton.deadlines,
            false,
        );
        self.observe(
            &baton.activation_id,
            cleanup_effect,
            &cleanup_observation(&cleanup),
        )?;
        let cleanup = match cleanup {
            Ok(proof) => proof,
            Err(_) => {
                return self.unsupported(execution, &baton, true, "current_cleanup_unproven");
            }
        };

        let switch_effect = Self::effect(
            &plan.current,
            &plan.candidate,
            baton.capability_generation,
            baton.candidate_process_generation,
        );
        self.intent(&baton.activation_id, &switch_effect)?;
        let switched = self
            .selector
            .apply_candidate_selector(plan, BootstrapPhaseV1::QuiescingCurrent);
        self.observe(
            &baton.activation_id,
            switch_effect,
            &selector_observation(&switched),
        )?;
        let selected = match switched {
            Ok(receipt) => receipt.after,
            Err(error) => {
                let observed = self.selector.observe_active_selector(plan);
                if observed.as_ref().is_ok_and(|value| {
                    value.selected_build_content_hash == plan.current.build_content_hash
                }) {
                    return self.unsupported(
                        execution,
                        &baton,
                        true,
                        "candidate_selector_not_applied",
                    );
                }
                self.advance(&baton.activation_id, BootstrapPhaseV1::CandidateSelected)?;
                return self.rollback(execution, &baton, &error.to_string());
            }
        };
        self.advance(&baton.activation_id, BootstrapPhaseV1::CandidateSelected)?;

        let launch_effect = Self::effect(
            &plan.candidate,
            &plan.candidate,
            baton.capability_generation,
            baton.candidate_process_generation,
        );
        self.intent(&baton.activation_id, &launch_effect)?;
        let spec = Self::launch_spec(
            execution,
            &baton,
            GenerationRoleV1::Candidate,
            plan.candidate.clone(),
            selected,
            cleanup,
            Self::stable_id(&baton.activation_id, "candidate-launch")?,
        );
        let launch = self.watchdog.launch_and_watch(&spec);
        self.observe(
            &baton.activation_id,
            launch_effect,
            &watchdog_observation(&launch),
        )?;
        self.advance(&baton.activation_id, BootstrapPhaseV1::CandidateLaunching)?;
        let focused_verification = match launch {
            Ok(GenerationWatchdogSuccessV1::CandidateVerified { verification, .. }) => {
                verification.focused_verification
            }
            _ => return self.rollback(execution, &baton, "candidate verification failed"),
        };
        self.advance(
            &baton.activation_id,
            BootstrapPhaseV1::AwaitingCandidateIdentity,
        )?;
        self.advance(&baton.activation_id, BootstrapPhaseV1::CandidateVerifying)?;
        self.advance(&baton.activation_id, BootstrapPhaseV1::Verified)?;
        let receipt = Self::receipt(
            execution,
            &baton,
            baton.candidate_process_generation,
            BootstrapResultKindV1::ActivatedVerified {
                focused_verification,
            },
            "verified",
        )?;
        self.journal
            .store_bootstrap_result(&receipt)
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        self.slots
            .mark_candidate_activated_verified()
            .map_err(|error| CoordinatorError::Slot(error.to_string()))?;
        self.journal
            .seal_terminal()
            .map_err(|error| CoordinatorError::Journal(error.to_string()))?;
        Ok(receipt)
    }

    fn recover_locked(
        &self,
        activation_id: &StableId,
        execution: Option<&ActivationExecutionV1>,
    ) -> Result<BootstrapResultV1, CoordinatorError> {
        let state = self.state(activation_id)?;
        if let Some(terminal) = state.terminal {
            return Ok(terminal);
        }
        let execution = execution.ok_or(CoordinatorError::MissingRecoveryContext)?;
        let baton = state.baton.ok_or(CoordinatorError::MissingBaton)?;
        self.validate_execution(execution, &baton)?;
        match state.phase {
            BootstrapPhaseV1::RolledBack => {
                let receipt = Self::receipt(
                    execution,
                    &baton,
                    baton.rollback_process_generation,
                    BootstrapResultKindV1::RolledBack {
                        reason: "recovered completed rollback".to_owned(),
                        rollback_evidence: vec![Self::rollback_evidence(&baton)?],
                    },
                    "rollback",
                )?;
                return self.seal(receipt);
            }
            BootstrapPhaseV1::Unsupported | BootstrapPhaseV1::AbortedBeforeSwitch => {
                let receipt = Self::receipt(
                    execution,
                    &baton,
                    baton.current_process_generation,
                    BootstrapResultKindV1::Unsupported {
                        reason: PlatformReasonV1 {
                            code: "recovered_pre_switch_terminal".to_owned(),
                            message: "Activation previously stopped before a verified switch."
                                .to_owned(),
                            next_steps: vec![
                                "Continue using the current verified build.".to_owned(),
                            ],
                        },
                    },
                    "unsupported",
                )?;
                return self.seal(receipt);
            }
            BootstrapPhaseV1::ManualRecoveryRequired => {
                let notice = state
                    .manual_recovery
                    .ok_or(CoordinatorError::MissingRecoveryContext)?;
                let receipt = Self::receipt(
                    execution,
                    &baton,
                    baton.rollback_process_generation,
                    BootstrapResultKindV1::ManualRecoveryRequired {
                        diagnostic_id: notice.diagnostic_id,
                        observed_slot_state: notice.observed_slot_state_hash,
                        instructions: notice.instructions,
                    },
                    "manual",
                )?;
                return self.seal(receipt);
            }
            _ => {}
        }
        if let Some(effect) = state.open_effect {
            self.reconcile_open_effect(execution, &baton, state.phase, effect)?;
        }
        if self.state(activation_id)?.phase != BootstrapPhaseV1::Recovering {
            self.advance(activation_id, BootstrapPhaseV1::Recovering)?;
        }
        let observed = self.selector.observe_active_selector(&execution.plan);
        match observed {
            Ok(value)
                if value.selected_build_content_hash
                    == execution.plan.current.build_content_hash =>
            {
                self.unsupported(execution, &baton, true, "recovered_before_switch")
            }
            Ok(value)
                if value.selected_build_content_hash
                    == execution.plan.candidate.build_content_hash
                    || value.selected_build_content_hash
                        == execution.plan.previous.build_content_hash =>
            {
                self.rollback(execution, &baton, "recovered incomplete activation")
            }
            other => self.manual(
                execution,
                &baton,
                ReasonCodeV1::AmbiguousSelectorState,
                &other.map_err(|error| error.to_string()),
            ),
        }
    }

    fn reconcile_open_effect(
        &self,
        execution: &ActivationExecutionV1,
        baton: &BatonAcceptedV1,
        phase: BootstrapPhaseV1,
        effect: BootstrapEffectV1,
    ) -> Result<(), CoordinatorError> {
        let plan = &execution.plan;
        if phase == BootstrapPhaseV1::QuiescingCurrent
            && effect.process_generation == baton.current_process_generation
        {
            let attempt = Self::stable_id(&baton.activation_id, "recover-current-cleanup")?;
            let result = self.watchdog.cleanup_generation(
                &baton.activation_id,
                &attempt,
                baton.current_process_generation,
                &baton.deadlines,
                false,
            );
            return self.observe(&baton.activation_id, effect, &cleanup_observation(&result));
        }
        if matches!(
            phase,
            BootstrapPhaseV1::CandidateSelected
                | BootstrapPhaseV1::CandidateLaunching
                | BootstrapPhaseV1::AwaitingCandidateIdentity
                | BootstrapPhaseV1::CandidateVerifying
        ) || (phase == BootstrapPhaseV1::RollingBack
            && effect.process_generation == baton.candidate_process_generation)
        {
            let attempt = Self::stable_id(&baton.activation_id, "recover-candidate-cleanup")?;
            let result = self.watchdog.cleanup_generation(
                &baton.activation_id,
                &attempt,
                baton.candidate_process_generation,
                &baton.deadlines,
                true,
            );
            return self.observe(&baton.activation_id, effect, &cleanup_observation(&result));
        }
        if matches!(
            phase,
            BootstrapPhaseV1::PreviousSelected | BootstrapPhaseV1::PreviousRelaunching
        ) {
            let attempt = Self::stable_id(&baton.activation_id, "recover-rollback-cleanup")?;
            let result = self.watchdog.cleanup_generation(
                &baton.activation_id,
                &attempt,
                baton.rollback_process_generation,
                &baton.deadlines,
                true,
            );
            return self.observe(&baton.activation_id, effect, &cleanup_observation(&result));
        }
        let selector = self.selector.observe_active_selector(plan);
        self.observe(
            &baton.activation_id,
            effect,
            &active_selector_observation(&selector),
        )
    }
}

impl ActivationControlPortV1 for ActivationRollbackCoordinator {
    fn execute_activation(
        &self,
        execution: &ActivationExecutionV1,
    ) -> Result<BootstrapResultV1, CoordinatorError> {
        let _flight = match self.flight.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) | Err(TryLockError::Poisoned(_)) => {
                return Err(CoordinatorError::Busy);
            }
        };
        self.execute_locked(execution)
    }

    fn recover_activation(
        &self,
        activation_id: &StableId,
        execution: Option<&ActivationExecutionV1>,
    ) -> Result<BootstrapResultV1, CoordinatorError> {
        let _flight = match self.flight.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) | Err(TryLockError::Poisoned(_)) => {
                return Err(CoordinatorError::Busy);
            }
        };
        self.recover_locked(activation_id, execution)
    }
}

impl BootstrapEnrollmentPortV1 for ActivationRollbackCoordinator {
    fn materialize(
        &self,
        request: &aworkit_protocol::ManagedLocalEnrollmentRequestV1,
        plan: &EnrollmentPlanV1,
    ) -> Result<EnrollmentPreparationV1, String> {
        let _flight = self
            .flight
            .try_lock()
            .map_err(|_| CoordinatorError::Busy.to_string())?;
        let mut unhashed_plan = plan.clone();
        let expected_plan_hash = unhashed_plan.plan_hash.clone();
        unhashed_plan.plan_hash.clear();
        if canonical_hash(&unhashed_plan).ok().as_ref() != Some(&expected_plan_hash) {
            return Err("enrollment plan hash is invalid".to_owned());
        }
        let provenance = BuildProvenanceV1 {
            source_revision: "projected".to_owned(),
            source_tree_hash: request.projected_provenance_hash.clone(),
            workspace_identity_hash: "managed-local-enrollment".to_owned(),
            toolchain_hash: "projected".to_owned(),
            build_manifest_hash: request.candidate_hash.clone(),
            provenance_hash: request.projected_provenance_hash.clone(),
        };
        let slot = self
            .slots
            .materialize_immutable_slot(&request.whole_bundle, &provenance)
            .map_err(|error| error.to_string())?;
        if slot.build_content_hash != request.candidate_hash
            || slot.root_identity_hash != plan.initial_active_slot_root_hash
        {
            return Err("materialized enrollment slot does not match its fixed plan".to_owned());
        }
        self.slots
            .set_initial_active(&slot)
            .map_err(|error| error.to_string())?;
        let enrollment_digest = canonical_hash(&(
            request,
            plan,
            &slot.build_content_hash,
            &slot.root_identity_hash,
        ))
        .map_err(|error| error.to_string())?;
        let preparation_id = Self::stable_id(&request.request_id, "enrollment")
            .map_err(|error| error.to_string())?;
        Ok(EnrollmentPreparationV1 {
            observation: crate::journal::EnrollmentObservationV1 {
                initial_active_bundle_hash: slot.build_content_hash,
                published_slot_verified: true,
            },
            prepared: aworkit_protocol::EnrollmentPreparedV1 {
                preparation_id,
                request_id: request.request_id.clone(),
                enrollment_digest,
                stable_launcher: "managed-local-stable-launcher".to_owned(),
                restart_instructions: vec![
                    "Close the current source-checkout application.".to_owned(),
                    "Restart through the stable managed-local launcher.".to_owned(),
                ],
            },
        })
    }
}

fn watchdog_observation(
    result: &Result<GenerationWatchdogSuccessV1, WatchdogFailureV1>,
) -> (bool, Option<&str>) {
    match result {
        Ok(_) => (true, None),
        Err(failure) => (false, Some(failure.reason_code.as_str())),
    }
}

fn cleanup_observation(result: &Result<ProcessTreeCleanupV1, WatchdogFailureV1>) -> (bool, String) {
    match result {
        Ok(proof) => (true, proof.proof_hash.clone()),
        Err(failure) => (false, failure.reason_code.clone()),
    }
}

fn selector_observation(
    result: &Result<crate::profile::SelectorMutationReceiptV1, crate::profile::ProfileError>,
) -> (bool, String) {
    match result {
        Ok(receipt) => (true, receipt.receipt_hash.clone()),
        Err(error) => (false, error.to_string()),
    }
}

fn selector_state_observation(
    result: &Result<crate::profile::ActiveSelectorObservationV1, crate::profile::ProfileError>,
) -> (bool, String) {
    match result {
        Ok(observation) => (true, observation.observation_hash.clone()),
        Err(error) => (false, error.to_string()),
    }
}

fn active_selector_observation(
    result: &Result<crate::profile::ActiveSelectorObservationV1, crate::profile::ProfileError>,
) -> (bool, String) {
    selector_state_observation(result)
}
