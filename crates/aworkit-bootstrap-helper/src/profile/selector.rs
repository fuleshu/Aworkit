//! Journal-fenced, idempotent managed-local selector adapter and test port.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aworkit_protocol::StableId;
use aworkit_trusted_core::PlatformReasonV1;

use crate::journal::{BootstrapPhaseV1, canonical_hash};
use crate::slots::{BuildSlotVerifyPortV1, OpenBuildSlotHandleV1};

use super::error::ProfileError;
use super::model::{
    ActivationPlanV1, ActiveSelectorObservationV1, NativeSelectorMutationOutcomeV1,
    SelectorMutationKindV1, SelectorMutationReceiptV1, SelectorMutationV1,
};
use super::ports::{PlatformActivationPortV1, SelectorMutationPortV1};

/// Platform-neutral enforcement around one native atomic selector primitive.
pub struct ManagedLocalSelectorAdapter {
    native: Arc<dyn SelectorMutationPortV1>,
    slots: Arc<dyn BuildSlotVerifyPortV1>,
    receipts: Mutex<HashMap<StableId, (String, SelectorMutationReceiptV1)>>,
}

impl ManagedLocalSelectorAdapter {
    #[must_use]
    pub fn new(
        native: Arc<dyn SelectorMutationPortV1>,
        slots: Arc<dyn BuildSlotVerifyPortV1>,
    ) -> Self {
        Self {
            native,
            slots,
            receipts: Mutex::new(HashMap::new()),
        }
    }

    fn mutation(
        plan: &ActivationPlanV1,
        kind: SelectorMutationKindV1,
        expected_phase: BootstrapPhaseV1,
    ) -> Result<SelectorMutationV1, ProfileError> {
        let (source, destination, generation) = match kind {
            SelectorMutationKindV1::SelectCandidate => (
                &plan.current,
                &plan.candidate,
                plan.candidate_process_generation,
            ),
            SelectorMutationKindV1::RestorePrevious => (
                &plan.candidate,
                &plan.previous,
                plan.rollback_process_generation,
            ),
        };
        let seed = canonical_hash(&(
            &plan.activation_id,
            kind,
            expected_phase,
            plan.capability_generation,
            &source.build_content_hash,
            &destination.build_content_hash,
            generation,
        ))
        .map_err(|_| ProfileError::Invalid("selector mutation id"))?;
        let mutation_id = StableId::parse(format!("selector.mutation.{}", &seed[7..39]))
            .map_err(|_| ProfileError::Invalid("selector mutation id"))?;
        let mut mutation = SelectorMutationV1 {
            mutation_id,
            activation_id: plan.activation_id.clone(),
            kind,
            expected_phase,
            capability_generation: plan.capability_generation,
            process_generation: generation,
            selector_identity_hash: plan.selector_identity_hash.clone(),
            expected_source_hash: source.build_content_hash.clone(),
            expected_source_root_identity_hash: source.root_identity_hash.clone(),
            destination_hash: destination.build_content_hash.clone(),
            destination_root_identity_hash: destination.root_identity_hash.clone(),
            mutation_hash: String::new(),
        };
        mutation.mutation_hash = canonical_hash(&mutation)
            .map_err(|_| ProfileError::Invalid("selector mutation hash"))?;
        Ok(mutation)
    }

    fn apply(
        &self,
        plan: &ActivationPlanV1,
        kind: SelectorMutationKindV1,
        expected_phase: BootstrapPhaseV1,
    ) -> Result<SelectorMutationReceiptV1, ProfileError> {
        let legal = matches!(
            (kind, expected_phase),
            (
                SelectorMutationKindV1::SelectCandidate,
                BootstrapPhaseV1::QuiescingCurrent
            ) | (
                SelectorMutationKindV1::RestorePrevious,
                BootstrapPhaseV1::RollingBack
            )
        );
        if !legal {
            return Err(ProfileError::Invalid("selector mutation phase"));
        }
        let mutation = Self::mutation(plan, kind, expected_phase)?;
        if let Some((hash, receipt)) = self
            .receipts
            .lock()
            .expect("selector receipt lock")
            .get(&mutation.mutation_id)
        {
            return if hash == &mutation.mutation_hash {
                Ok(receipt.clone())
            } else {
                Err(ProfileError::MutationReplay)
            };
        }
        let (source, destination) = match kind {
            SelectorMutationKindV1::SelectCandidate => (&plan.current, &plan.candidate),
            SelectorMutationKindV1::RestorePrevious => (&plan.candidate, &plan.previous),
        };
        self.slots
            .reverify_opened_slot(&source.handle)
            .map_err(|error| ProfileError::Slot(error.to_string()))?;
        self.slots
            .reverify_opened_slot(&destination.handle)
            .map_err(|error| ProfileError::Slot(error.to_string()))?;
        let before = self.native.observe()?;
        if before.selector_identity_hash != plan.selector_identity_hash
            || before.capability_generation != plan.capability_generation
            || before.selected_build_content_hash != source.build_content_hash
            || before.selected_root_identity_hash != source.root_identity_hash
        {
            return Err(ProfileError::SelectorDrift);
        }
        let after = match self.native.atomic_replace(&mutation)? {
            NativeSelectorMutationOutcomeV1::Applied(after) => after,
            NativeSelectorMutationOutcomeV1::DefinitelyNotApplied(reason) => {
                return Err(ProfileError::Selector(reason.code));
            }
            NativeSelectorMutationOutcomeV1::Ambiguous => {
                return Err(ProfileError::AmbiguousSelector);
            }
        };
        if after.selector_identity_hash != plan.selector_identity_hash
            || after.capability_generation != plan.capability_generation
            || after.selected_build_content_hash != destination.build_content_hash
            || after.selected_root_identity_hash != destination.root_identity_hash
        {
            return Err(ProfileError::AmbiguousSelector);
        }
        let reopened = self.native.observe()?;
        if reopened != after {
            return Err(ProfileError::AmbiguousSelector);
        }
        let mut receipt = SelectorMutationReceiptV1 {
            mutation_id: mutation.mutation_id,
            activation_id: plan.activation_id.clone(),
            kind,
            before,
            after,
            mutation_hash: mutation.mutation_hash,
            receipt_hash: String::new(),
        };
        receipt.receipt_hash =
            canonical_hash(&receipt).map_err(|_| ProfileError::Invalid("selector receipt hash"))?;
        self.receipts.lock().expect("selector receipt lock").insert(
            receipt.mutation_id.clone(),
            (receipt.mutation_hash.clone(), receipt.clone()),
        );
        Ok(receipt)
    }
}

impl PlatformActivationPortV1 for ManagedLocalSelectorAdapter {
    fn observe_active_selector(
        &self,
        plan: &ActivationPlanV1,
    ) -> Result<ActiveSelectorObservationV1, ProfileError> {
        let observation = self.native.observe()?;
        if observation.selector_identity_hash != plan.selector_identity_hash
            || observation.capability_generation != plan.capability_generation
        {
            return Err(ProfileError::CapabilityDrift);
        }
        Ok(observation)
    }

    fn apply_candidate_selector(
        &self,
        plan: &ActivationPlanV1,
        expected_phase: BootstrapPhaseV1,
    ) -> Result<SelectorMutationReceiptV1, ProfileError> {
        self.apply(
            plan,
            SelectorMutationKindV1::SelectCandidate,
            expected_phase,
        )
    }

    fn restore_previous_selector(
        &self,
        plan: &ActivationPlanV1,
        expected_phase: BootstrapPhaseV1,
    ) -> Result<SelectorMutationReceiptV1, ProfileError> {
        self.apply(
            plan,
            SelectorMutationKindV1::RestorePrevious,
            expected_phase,
        )
    }

    fn verify_selector(
        &self,
        plan: &ActivationPlanV1,
        expected: &OpenBuildSlotHandleV1,
    ) -> Result<ActiveSelectorObservationV1, ProfileError> {
        self.slots
            .reverify_opened_slot(expected)
            .map_err(|error| ProfileError::Slot(error.to_string()))?;
        let observation = self.observe_active_selector(plan)?;
        if observation.selected_build_content_hash != expected.build_content_hash
            || observation.selected_root_identity_hash != expected.root_identity_hash
        {
            return Err(ProfileError::SelectorDrift);
        }
        Ok(observation)
    }
}

/// Hermetic native selector with exact idempotency and ambiguity injection.
pub struct HermeticSelectorPort {
    state: Mutex<HermeticSelectorState>,
}

struct HermeticSelectorState {
    current: ActiveSelectorObservationV1,
    mutations: HashMap<StableId, (String, NativeSelectorMutationOutcomeV1)>,
    next_ambiguous: bool,
    next_definitely_not_applied: bool,
}

impl HermeticSelectorPort {
    #[must_use]
    pub fn new(current: ActiveSelectorObservationV1) -> Self {
        Self {
            state: Mutex::new(HermeticSelectorState {
                current,
                mutations: HashMap::new(),
                next_ambiguous: false,
                next_definitely_not_applied: false,
            }),
        }
    }

    pub fn fail_next_ambiguous(&self) {
        self.state.lock().expect("selector lock").next_ambiguous = true;
    }

    pub fn fail_next_definitely_not_applied(&self) {
        self.state
            .lock()
            .expect("selector lock")
            .next_definitely_not_applied = true;
    }

    pub fn replace_observation(&self, observation: ActiveSelectorObservationV1) {
        self.state.lock().expect("selector lock").current = observation;
    }
}

impl SelectorMutationPortV1 for HermeticSelectorPort {
    fn observe(&self) -> Result<ActiveSelectorObservationV1, ProfileError> {
        Ok(self.state.lock().expect("selector lock").current.clone())
    }

    fn atomic_replace(
        &self,
        mutation: &SelectorMutationV1,
    ) -> Result<NativeSelectorMutationOutcomeV1, ProfileError> {
        let mut state = self.state.lock().expect("selector lock");
        if let Some((hash, outcome)) = state.mutations.get(&mutation.mutation_id) {
            if hash == &mutation.mutation_hash {
                return Ok(outcome.clone());
            }
            return Err(ProfileError::MutationReplay);
        }
        let mut unhashed = mutation.clone();
        unhashed.mutation_hash.clear();
        if canonical_hash(&unhashed).map_err(|_| ProfileError::Invalid("mutation hash"))?
            != mutation.mutation_hash
        {
            return Err(ProfileError::MutationReplay);
        }
        if state.current.selector_identity_hash != mutation.selector_identity_hash
            || state.current.capability_generation != mutation.capability_generation
            || state.current.selected_build_content_hash != mutation.expected_source_hash
            || state.current.selected_root_identity_hash
                != mutation.expected_source_root_identity_hash
        {
            return Err(ProfileError::SelectorDrift);
        }
        let outcome = if state.next_ambiguous {
            state.next_ambiguous = false;
            NativeSelectorMutationOutcomeV1::Ambiguous
        } else if state.next_definitely_not_applied {
            state.next_definitely_not_applied = false;
            NativeSelectorMutationOutcomeV1::DefinitelyNotApplied(PlatformReasonV1 {
                code: "injected_not_applied".to_owned(),
                message: "selector was definitely not changed".to_owned(),
                next_steps: Vec::new(),
            })
        } else {
            let mut after = ActiveSelectorObservationV1 {
                selector_identity_hash: mutation.selector_identity_hash.clone(),
                selected_build_content_hash: mutation.destination_hash.clone(),
                selected_root_identity_hash: mutation.destination_root_identity_hash.clone(),
                capability_generation: mutation.capability_generation,
                observation_hash: String::new(),
            };
            after.observation_hash = canonical_hash(&after)
                .map_err(|_| ProfileError::Invalid("selector observation hash"))?;
            state.current = after.clone();
            NativeSelectorMutationOutcomeV1::Applied(after)
        };
        state.mutations.insert(
            mutation.mutation_id.clone(),
            (mutation.mutation_hash.clone(), outcome.clone()),
        );
        Ok(outcome)
    }
}
