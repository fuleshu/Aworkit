//! Native process mechanics and platform-neutral watchdog contract.

use aworkit_protocol::ProcessGeneration;
use aworkit_trusted_core::ManualRecoveryNoticeV1;

use super::model::*;

/// Native M12.2 process/IPC adapter. Every wait receives a bounded duration.
pub trait PlatformProcessPortV1: Send + Sync {
    fn request_cooperative_shutdown(&self, generation: ProcessGeneration) -> Result<(), String>;
    fn await_tree_exit(
        &self,
        generation: ProcessGeneration,
        timeout_ms: u64,
    ) -> Result<bool, String>;
    fn force_terminate_tree(
        &self,
        generation: ProcessGeneration,
        timeout_ms: u64,
    ) -> Result<(), String>;
    fn prove_tree_empty(
        &self,
        generation: ProcessGeneration,
    ) -> Result<ProcessTreeCleanupV1, String>;
    fn spawn_verified(
        &self,
        request: &PlatformLaunchRequestV1,
    ) -> Result<LaunchObservationV1, String>;
    fn await_identity_handshake(
        &self,
        process_tree: &ProcessTreeHandleV1,
        timeout_ms: u64,
    ) -> Result<Option<GenerationHandshakeV1>, String>;
    fn health_snapshot(
        &self,
        process_tree: &ProcessTreeHandleV1,
        timeout_ms: u64,
    ) -> Result<Option<GenerationHealthV1>, String>;
    fn handoff_focused_verification(
        &self,
        process_tree: &ProcessTreeHandleV1,
        verification_plan_hash: &str,
    ) -> Result<(), String>;
    fn await_focused_verification(
        &self,
        process_tree: &ProcessTreeHandleV1,
        timeout_ms: u64,
    ) -> Result<Option<FocusedVerificationResultV1>, String>;
}

/// Coordinator-facing policy with no platform-specific process operation.
pub trait ApplicationLaunchWatchdogPortV1: Send + Sync {
    fn cleanup_generation(
        &self,
        activation_id: &aworkit_protocol::StableId,
        attempt_id: &aworkit_protocol::StableId,
        generation: ProcessGeneration,
        deadlines: &aworkit_trusted_core::BootstrapDeadlinesV1,
        rollback_required: bool,
    ) -> Result<ProcessTreeCleanupV1, WatchdogFailureV1>;

    fn launch_and_watch(
        &self,
        spec: &GenerationLaunchSpecV1,
    ) -> Result<GenerationWatchdogSuccessV1, WatchdogFailureV1>;

    fn stable_launcher_notice(&self, notice: &ManualRecoveryNoticeV1) -> StableLauncherNoticeV1;
}
