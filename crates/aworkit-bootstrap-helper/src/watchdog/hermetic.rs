//! Hermetic native process/IPC adapter with ordered operations and faults.

use std::collections::HashMap;
use std::sync::Mutex;

use aworkit_protocol::{
    CapabilityOutcomeClassV1, CapabilityOutcomeV1, ProcessGeneration, StableId,
};

use crate::journal::canonical_hash;

use super::model::*;
use super::ports::PlatformProcessPortV1;

/// Script for one generation launch and cleanup lifecycle.
#[derive(Clone, Debug)]
pub struct HermeticGenerationScriptV1 {
    pub spawn_fails: bool,
    pub handshake_available: bool,
    pub handshake_identity_mismatch: bool,
    pub health_available: bool,
    pub healthy: bool,
    pub verification_available: bool,
    pub verification_passed: bool,
    pub cooperative_exit: bool,
    pub force_termination_succeeds: bool,
    pub orphan_risk: bool,
    pub descendants: u32,
}

impl Default for HermeticGenerationScriptV1 {
    fn default() -> Self {
        Self {
            spawn_fails: false,
            handshake_available: true,
            handshake_identity_mismatch: false,
            health_available: true,
            healthy: true,
            verification_available: true,
            verification_passed: true,
            cooperative_exit: true,
            force_termination_succeeds: true,
            orphan_risk: false,
            descendants: 1,
        }
    }
}

/// Deterministic process double. `events()` exposes cleanup/launch ordering.
#[derive(Default)]
pub struct HermeticPlatformProcessPort {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    scripts: HashMap<u64, HermeticGenerationScriptV1>,
    launches: HashMap<u64, PlatformLaunchRequestV1>,
    running: HashMap<u64, bool>,
    forced: HashMap<u64, bool>,
    events: Vec<String>,
}

impl HermeticPlatformProcessPort {
    pub fn script(
        &self,
        generation: ProcessGeneration,
        script: HermeticGenerationScriptV1,
        initially_running: bool,
    ) {
        let mut state = self.state.lock().expect("process state lock");
        state.scripts.insert(generation.0, script);
        state.running.insert(generation.0, initially_running);
    }

    #[must_use]
    pub fn events(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("process state lock")
            .events
            .clone()
    }

    fn script_for(state: &State, generation: ProcessGeneration) -> HermeticGenerationScriptV1 {
        state
            .scripts
            .get(&generation.0)
            .cloned()
            .unwrap_or_default()
    }
}

impl PlatformProcessPortV1 for HermeticPlatformProcessPort {
    fn request_cooperative_shutdown(&self, generation: ProcessGeneration) -> Result<(), String> {
        self.state
            .lock()
            .expect("process state lock")
            .events
            .push(format!("shutdown:{}", generation.0));
        Ok(())
    }

    fn await_tree_exit(
        &self,
        generation: ProcessGeneration,
        _timeout_ms: u64,
    ) -> Result<bool, String> {
        let mut state = self.state.lock().expect("process state lock");
        state.events.push(format!("await_exit:{}", generation.0));
        let exited = Self::script_for(&state, generation).cooperative_exit;
        if exited {
            state.running.insert(generation.0, false);
        }
        Ok(exited)
    }

    fn force_terminate_tree(
        &self,
        generation: ProcessGeneration,
        _timeout_ms: u64,
    ) -> Result<(), String> {
        let mut state = self.state.lock().expect("process state lock");
        state.events.push(format!("force:{}", generation.0));
        if !Self::script_for(&state, generation).force_termination_succeeds {
            return Err("forced termination failed".to_owned());
        }
        state.running.insert(generation.0, false);
        state.forced.insert(generation.0, true);
        Ok(())
    }

    fn prove_tree_empty(
        &self,
        generation: ProcessGeneration,
    ) -> Result<ProcessTreeCleanupV1, String> {
        let mut state = self.state.lock().expect("process state lock");
        state.events.push(format!("prove_empty:{}", generation.0));
        let script = Self::script_for(&state, generation);
        let mut proof = ProcessTreeCleanupV1 {
            process_generation: generation,
            cooperative_requested: true,
            forced_termination_used: state.forced.get(&generation.0).copied().unwrap_or(false),
            descendants_observed: script.descendants,
            tree_empty: !state.running.get(&generation.0).copied().unwrap_or(false),
            orphan_risk: script.orphan_risk,
            proof_hash: String::new(),
        };
        proof.proof_hash = canonical_hash(&proof).map_err(|error| error.to_string())?;
        Ok(proof)
    }

    fn spawn_verified(
        &self,
        request: &PlatformLaunchRequestV1,
    ) -> Result<LaunchObservationV1, String> {
        let mut state = self.state.lock().expect("process state lock");
        state
            .events
            .push(format!("spawn:{}", request.process_generation.0));
        if Self::script_for(&state, request.process_generation).spawn_fails {
            return Err("spawn failed".to_owned());
        }
        state
            .launches
            .insert(request.process_generation.0, request.clone());
        state.running.insert(request.process_generation.0, true);
        let mut launch = LaunchObservationV1 {
            attempt_id: request.attempt_id.clone(),
            process_tree: ProcessTreeHandleV1 {
                handle_id: StableId::parse(format!(
                    "process.tree.{}",
                    request.process_generation.0
                ))
                .map_err(|error| error.to_string())?,
                process_generation: request.process_generation,
                root_process_identity_hash: request.slot_handle.build_content_hash.clone(),
                containment_identity_hash: request.slot_handle.root_identity_hash.clone(),
            },
            executable_hash: request.slot_handle.build_content_hash.clone(),
            slot_root_identity_hash: request.slot_handle.root_identity_hash.clone(),
            observed_at_monotonic_ms: 1,
            observation_hash: String::new(),
        };
        launch.observation_hash = canonical_hash(&launch).map_err(|error| error.to_string())?;
        Ok(launch)
    }

    fn await_identity_handshake(
        &self,
        process_tree: &ProcessTreeHandleV1,
        _timeout_ms: u64,
    ) -> Result<Option<GenerationHandshakeV1>, String> {
        let mut state = self.state.lock().expect("process state lock");
        state.events.push(format!(
            "await_handshake:{}",
            process_tree.process_generation.0
        ));
        let script = Self::script_for(&state, process_tree.process_generation);
        if !script.handshake_available {
            return Ok(None);
        }
        let request = state
            .launches
            .get(&process_tree.process_generation.0)
            .cloned()
            .ok_or_else(|| "generation was not launched".to_owned())?;
        let mut handshake = GenerationHandshakeV1 {
            activation_id: request.activation_id.clone(),
            attempt_id: request.attempt_id,
            installation_id: request.installation_id,
            enrollment_digest: request.enrollment_digest,
            capability_generation: request.capability_generation,
            capability_digest: request.capability_digest,
            launch_nonce_hash: request.launch_nonce_hash,
            executable_hash: request.slot_handle.build_content_hash,
            slot_root_identity_hash: request.slot_handle.root_identity_hash,
            helper_protocol_version: request.helper_protocol_version,
            verification_plan_hash: request.verification_plan_hash.clone(),
            mode: request.mode,
            process_generation: request.process_generation,
            handshake_hash: String::new(),
        };
        if script.handshake_identity_mismatch {
            handshake.executable_hash =
                "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
                    .to_owned();
        }
        handshake.handshake_hash = canonical_hash(&handshake).map_err(|error| error.to_string())?;
        Ok(Some(handshake))
    }

    fn health_snapshot(
        &self,
        process_tree: &ProcessTreeHandleV1,
        _timeout_ms: u64,
    ) -> Result<Option<GenerationHealthV1>, String> {
        let mut state = self.state.lock().expect("process state lock");
        state
            .events
            .push(format!("health:{}", process_tree.process_generation.0));
        let script = Self::script_for(&state, process_tree.process_generation);
        if !script.health_available {
            return Ok(None);
        }
        let request = state
            .launches
            .get(&process_tree.process_generation.0)
            .ok_or_else(|| "generation was not launched".to_owned())?;
        let mut health = GenerationHealthV1 {
            attempt_id: request.attempt_id.clone(),
            process_generation: request.process_generation,
            healthy: script.healthy,
            heartbeat_sequence: 1,
            observation_hash: String::new(),
        };
        health.observation_hash = canonical_hash(&health).map_err(|error| error.to_string())?;
        Ok(Some(health))
    }

    fn handoff_focused_verification(
        &self,
        process_tree: &ProcessTreeHandleV1,
        _verification_plan_hash: &str,
    ) -> Result<(), String> {
        self.state
            .lock()
            .expect("process state lock")
            .events
            .push(format!(
                "verification_handoff:{}",
                process_tree.process_generation.0
            ));
        Ok(())
    }

    fn await_focused_verification(
        &self,
        process_tree: &ProcessTreeHandleV1,
        _timeout_ms: u64,
    ) -> Result<Option<FocusedVerificationResultV1>, String> {
        let mut state = self.state.lock().expect("process state lock");
        state.events.push(format!(
            "await_verification:{}",
            process_tree.process_generation.0
        ));
        let script = Self::script_for(&state, process_tree.process_generation);
        if !script.verification_available {
            return Ok(None);
        }
        let request = state
            .launches
            .get(&process_tree.process_generation.0)
            .cloned()
            .ok_or_else(|| "generation was not launched".to_owned())?;
        let mut result = FocusedVerificationResultV1 {
            activation_id: request.activation_id.clone(),
            attempt_id: request.attempt_id,
            process_generation: request.process_generation,
            verification_plan_hash: request.verification_plan_hash.clone(),
            passed: script.verification_passed,
            outcome: CapabilityOutcomeV1 {
                outcome_id: StableId::parse(format!(
                    "verification.outcome.{}",
                    request.process_generation.0
                ))
                .map_err(|error| error.to_string())?,
                invocation_id: StableId::parse(format!(
                    "verification.invocation.{}",
                    request.process_generation.0
                ))
                .map_err(|error| error.to_string())?,
                class: if script.verification_passed {
                    CapabilityOutcomeClassV1::Success
                } else {
                    CapabilityOutcomeClassV1::FailedKnownStarted
                },
                retry_safe_proof: false,
                payload: serde_json::json!({"passed": script.verification_passed}),
                usage: None,
            },
            focused_verification: {
                let evidence_artifact_hash = canonical_hash(&(
                    &request.activation_id,
                    request.process_generation,
                    &request.verification_plan_hash,
                ))
                .map_err(|error| error.to_string())?;
                let mut evidence = aworkit_trusted_core::FocusedVerificationEvidenceV1 {
                    plan_id: request.verification_plan_id.clone(),
                    plan_hash: request.verification_plan_hash.clone(),
                    results: request
                        .verification_check_ids
                        .iter()
                        .enumerate()
                        .map(|(index, check_id)| {
                            Ok(aworkit_trusted_core::FocusedVerificationCheckResultV1 {
                                check_id: check_id.clone(),
                                passed: script.verification_passed,
                                summary: if script.verification_passed {
                                    "focused verification passed".to_owned()
                                } else {
                                    "focused verification failed".to_owned()
                                },
                                evidence: vec![aworkit_trusted_core::RepairArtifactRefV1 {
                                    artifact_id: StableId::parse(format!(
                                        "verification.evidence.{}.{index}",
                                        request.process_generation.0
                                    ))
                                    .map_err(|error| error.to_string())?,
                                    content_hash: evidence_artifact_hash.clone(),
                                    byte_size: 1,
                                    media_type: "application/json".to_owned(),
                                    logical_name: format!("focused-check-{index}.json"),
                                }],
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()?,
                    evidence_hash: String::new(),
                };
                evidence.evidence_hash =
                    aworkit_trusted_core::focused_verification_evidence_hash_v1(&evidence)
                        .map_err(|error| error.to_string())?;
                evidence
            },
            result_hash: String::new(),
        };
        result.result_hash = canonical_hash(&result).map_err(|error| error.to_string())?;
        Ok(Some(result))
    }
}
