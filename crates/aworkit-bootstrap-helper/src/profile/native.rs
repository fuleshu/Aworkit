//! Durable helper-controlled managed-local selector primitive.

use std::{path::Path, sync::Mutex};

use aworkit_process::filesystem::{AnchoredDirectory, AnchoredRelativePath};
use sha2::Digest;

use crate::journal::canonical_hash;

use super::{
    ActiveSelectorObservationV1, NativeSelectorMutationOutcomeV1, ProfileError,
    SelectorMutationPortV1, SelectorMutationV1,
};

const MAX_SELECTOR_BYTES: usize = 64 * 1024;

/// One fixed selector beneath the helper-controlled managed root.
pub struct NativeSelectorPort {
    root: AnchoredDirectory,
    selector: AnchoredRelativePath,
    mutation_lock: Mutex<()>,
}

impl NativeSelectorPort {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ProfileError> {
        let root = AnchoredDirectory::open(root)
            .map_err(|error| ProfileError::Selector(error.to_string()))?;
        if !root.capability_report().supports_managed_publication() {
            return Err(ProfileError::Unsupported(
                "managed root lacks native durability or ownership guarantees",
            ));
        }
        Ok(Self {
            root,
            selector: AnchoredRelativePath::parse(".managed/selector/active.json")
                .map_err(|_| ProfileError::Invalid("selector path"))?,
            mutation_lock: Mutex::new(()),
        })
    }

    /// Creates the selector exactly once during managed-local enrollment.
    pub fn initialize(
        &self,
        mut observation: ActiveSelectorObservationV1,
    ) -> Result<ActiveSelectorObservationV1, ProfileError> {
        observation.observation_hash.clear();
        observation.observation_hash = canonical_hash(&observation)
            .map_err(|_| ProfileError::Invalid("selector observation"))?;
        let bytes = serde_json::to_vec(&observation)
            .map_err(|_| ProfileError::Invalid("selector observation"))?;
        self.root
            .create_new_durable(&self.selector, &bytes)
            .map_err(|error| ProfileError::Selector(error.to_string()))?;
        Ok(observation)
    }

    fn read(&self) -> Result<ActiveSelectorObservationV1, ProfileError> {
        let bytes = self
            .root
            .read_bounded(&self.selector, MAX_SELECTOR_BYTES)
            .map_err(|error| ProfileError::Selector(error.to_string()))?;
        let observation: ActiveSelectorObservationV1 = serde_json::from_slice(&bytes)
            .map_err(|_| ProfileError::Invalid("selector observation"))?;
        let mut unhashed = observation.clone();
        unhashed.observation_hash.clear();
        let expected =
            canonical_hash(&unhashed).map_err(|_| ProfileError::Invalid("selector observation"))?;
        if expected != observation.observation_hash {
            return Err(ProfileError::SelectorDrift);
        }
        Ok(observation)
    }
}

impl SelectorMutationPortV1 for NativeSelectorPort {
    fn observe(&self) -> Result<ActiveSelectorObservationV1, ProfileError> {
        self.read()
    }

    fn atomic_replace(
        &self,
        mutation: &SelectorMutationV1,
    ) -> Result<NativeSelectorMutationOutcomeV1, ProfileError> {
        let _guard = self.mutation_lock.lock().expect("native selector lock");
        let mut unhashed = mutation.clone();
        unhashed.mutation_hash.clear();
        if canonical_hash(&unhashed).map_err(|_| ProfileError::Invalid("mutation hash"))?
            != mutation.mutation_hash
        {
            return Err(ProfileError::MutationReplay);
        }
        let before = self.read()?;
        if before.selector_identity_hash != mutation.selector_identity_hash
            || before.capability_generation != mutation.capability_generation
        {
            return Err(ProfileError::CapabilityDrift);
        }
        if before.selected_build_content_hash == mutation.destination_hash
            && before.selected_root_identity_hash == mutation.destination_root_identity_hash
        {
            return Ok(NativeSelectorMutationOutcomeV1::Applied(before));
        }
        if before.selected_build_content_hash != mutation.expected_source_hash
            || before.selected_root_identity_hash != mutation.expected_source_root_identity_hash
        {
            return Err(ProfileError::SelectorDrift);
        }
        let mut after = ActiveSelectorObservationV1 {
            selector_identity_hash: mutation.selector_identity_hash.clone(),
            selected_build_content_hash: mutation.destination_hash.clone(),
            selected_root_identity_hash: mutation.destination_root_identity_hash.clone(),
            capability_generation: mutation.capability_generation,
            observation_hash: String::new(),
        };
        after.observation_hash =
            canonical_hash(&after).map_err(|_| ProfileError::Invalid("selector observation"))?;
        let bytes = serde_json::to_vec(&after)
            .map_err(|_| ProfileError::Invalid("selector observation"))?;
        self.root
            .replace_durable_expected(
                &self.selector,
                Some(&format!(
                    "sha256:{:x}",
                    sha2::Sha256::digest(
                        serde_json::to_vec(&before)
                            .map_err(|_| ProfileError::Invalid("selector observation"))?
                    )
                )),
                &bytes,
            )
            .map_err(|error| ProfileError::Selector(error.to_string()))?;
        match self.read() {
            Ok(reopened) if reopened == after => {
                Ok(NativeSelectorMutationOutcomeV1::Applied(after))
            }
            Ok(_) => Ok(NativeSelectorMutationOutcomeV1::Ambiguous),
            Err(_) => Ok(NativeSelectorMutationOutcomeV1::Ambiguous),
        }
    }
}

#[cfg(test)]
mod tests {
    use aworkit_protocol::{ProcessGeneration, StableId};

    use crate::journal::BootstrapPhaseV1;

    use super::*;

    #[test]
    fn native_selector_is_durable_expected_source_guarded_and_reopenable() {
        let temporary = tempfile::tempdir().expect("managed root");
        let selector = NativeSelectorPort::open(temporary.path()).expect("native selector");
        let initial = selector
            .initialize(ActiveSelectorObservationV1 {
                selector_identity_hash: "selector-id".to_owned(),
                selected_build_content_hash: "build-a".to_owned(),
                selected_root_identity_hash: "root-a".to_owned(),
                capability_generation: 7,
                observation_hash: String::new(),
            })
            .expect("initialize selector");
        assert_eq!(selector.observe().expect("observe"), initial);

        let mut mutation = SelectorMutationV1 {
            mutation_id: StableId::parse("selector.native.test").expect("mutation id"),
            activation_id: StableId::parse("activation.native.test").expect("activation id"),
            kind: super::super::SelectorMutationKindV1::SelectCandidate,
            expected_phase: BootstrapPhaseV1::QuiescingCurrent,
            capability_generation: 7,
            process_generation: ProcessGeneration(8),
            selector_identity_hash: "selector-id".to_owned(),
            expected_source_hash: "build-a".to_owned(),
            expected_source_root_identity_hash: "root-a".to_owned(),
            destination_hash: "build-b".to_owned(),
            destination_root_identity_hash: "root-b".to_owned(),
            mutation_hash: String::new(),
        };
        mutation.mutation_hash = canonical_hash(&mutation).expect("mutation hash");
        let applied = selector.atomic_replace(&mutation).expect("replace");
        assert!(matches!(
            applied,
            NativeSelectorMutationOutcomeV1::Applied(_)
        ));
        assert_eq!(
            selector
                .observe()
                .expect("reopen")
                .selected_build_content_hash,
            "build-b"
        );
        assert!(matches!(
            selector
                .atomic_replace(&mutation)
                .expect("idempotent reconciliation"),
            NativeSelectorMutationOutcomeV1::Applied(_)
        ));
    }
}
