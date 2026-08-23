//! Observation and selector ports for the managed-local profile.

use crate::journal::BootstrapPhaseV1;
use crate::slots::OpenBuildSlotHandleV1;

use super::error::ProfileError;
use super::model::{
    ActivationPlanV1, ActiveSelectorObservationV1, NativeSelectorMutationOutcomeV1,
    ProfileRuntimeObservationsV1, SelectorMutationReceiptV1, SelectorMutationV1,
};

/// Fresh platform facts used for capability classification and revalidation.
pub trait ProfileObservationPortV1: Send + Sync {
    fn observe(&self) -> Result<ProfileRuntimeObservationsV1, ProfileError>;
}

/// Native M12.1 selector primitive over one fixed helper-controlled selector.
pub trait SelectorMutationPortV1: Send + Sync {
    fn observe(&self) -> Result<ActiveSelectorObservationV1, ProfileError>;
    fn atomic_replace(
        &self,
        mutation: &SelectorMutationV1,
    ) -> Result<NativeSelectorMutationOutcomeV1, ProfileError>;
}

/// Platform-neutral selector contract used by the coordinator.
pub trait PlatformActivationPortV1: Send + Sync {
    fn observe_active_selector(
        &self,
        plan: &ActivationPlanV1,
    ) -> Result<ActiveSelectorObservationV1, ProfileError>;

    fn apply_candidate_selector(
        &self,
        plan: &ActivationPlanV1,
        expected_phase: BootstrapPhaseV1,
    ) -> Result<SelectorMutationReceiptV1, ProfileError>;

    fn restore_previous_selector(
        &self,
        plan: &ActivationPlanV1,
        expected_phase: BootstrapPhaseV1,
    ) -> Result<SelectorMutationReceiptV1, ProfileError>;

    fn verify_selector(
        &self,
        plan: &ActivationPlanV1,
        expected: &OpenBuildSlotHandleV1,
    ) -> Result<ActiveSelectorObservationV1, ProfileError>;
}
