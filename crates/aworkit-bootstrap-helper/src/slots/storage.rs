//! Hermetic anchored immutable-slot storage for tests and coordination.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use aworkit_protocol::StableId;

use super::error::BuildSlotError;
use super::model::{
    OpenBuildSlotHandleV1, SlotManifestV1, SlotMaterializationV1, StoredSlotObservationV1,
};
use super::ports::BuildSlotStoragePortV1;

/// In-memory storage that models unreachable temporaries, atomic publication,
/// immutable content-addressed slots, and anchored handles.
#[derive(Debug)]
pub struct InMemoryBuildSlotStorage {
    state: Mutex<State>,
    owner_identity_hash: String,
    volume_identity_hash: String,
}

#[derive(Debug, Default)]
struct State {
    next_id: u64,
    materializations: HashMap<StableId, Materialization>,
    slots: HashMap<String, StoredSlot>,
    handles: HashMap<StableId, HandleBinding>,
    guarantees: Guarantees,
}

#[derive(Debug, Default)]
struct Guarantees {
    per_user_owned: bool,
    same_volume: bool,
    immutable: bool,
    no_follow: bool,
}

#[derive(Debug)]
struct Materialization {
    build_content_hash: String,
    entries: BTreeMap<String, Vec<u8>>,
}

#[derive(Clone, Debug)]
struct StoredSlot {
    manifest: SlotManifestV1,
    entries: BTreeMap<String, Vec<u8>>,
    root_identity_hash: String,
}

#[derive(Debug)]
struct HandleBinding {
    build_content_hash: String,
    root_identity_hash: String,
    manifest_hash: String,
    verification_generation: u64,
}

impl Default for InMemoryBuildSlotStorage {
    fn default() -> Self {
        Self::new(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
    }
}

impl InMemoryBuildSlotStorage {
    #[must_use]
    pub fn new(
        owner_identity_hash: impl Into<String>,
        volume_identity_hash: impl Into<String>,
    ) -> Self {
        Self {
            state: Mutex::new(State {
                guarantees: Guarantees {
                    per_user_owned: true,
                    same_volume: true,
                    immutable: true,
                    no_follow: true,
                },
                ..State::default()
            }),
            owner_identity_hash: owner_identity_hash.into(),
            volume_identity_hash: volume_identity_hash.into(),
        }
    }

    /// Fault injection: changes the named immutable byte vector behind an open
    /// handle so last-moment revalidation must fail.
    pub fn corrupt_entry(&self, build_hash: &str, path: &str) {
        if let Some(bytes) = self
            .state
            .lock()
            .expect("slot storage lock")
            .slots
            .get_mut(build_hash)
            .and_then(|slot| slot.entries.get_mut(path))
        {
            if let Some(first) = bytes.first_mut() {
                *first ^= 0xff;
            }
        }
    }

    /// Fault injection for ownership, volume, immutability, and no-follow
    /// degradation checks.
    pub fn set_guarantees(
        &self,
        per_user_owned: bool,
        same_volume: bool,
        immutable: bool,
        no_follow: bool,
    ) {
        self.state.lock().expect("slot storage lock").guarantees = Guarantees {
            per_user_owned,
            same_volume,
            immutable,
            no_follow,
        };
    }

    fn observation(
        &self,
        state: &mut State,
        build_content_hash: &str,
        existing: Option<&OpenBuildSlotHandleV1>,
    ) -> Result<StoredSlotObservationV1, BuildSlotError> {
        let slot = state
            .slots
            .get(build_content_hash)
            .cloned()
            .ok_or(BuildSlotError::NotFound)?;
        state.next_id = state.next_id.saturating_add(1);
        let verification_generation = existing
            .map(|handle| handle.verification_generation)
            .unwrap_or(state.next_id);
        let handle_id = existing
            .map(|handle| handle.handle_id.clone())
            .unwrap_or_else(|| {
                StableId::parse(format!("slot.handle.{}", state.next_id))
                    .expect("generated handle id")
            });
        let handle = OpenBuildSlotHandleV1 {
            handle_id: handle_id.clone(),
            build_content_hash: build_content_hash.to_owned(),
            root_identity_hash: slot.root_identity_hash.clone(),
            manifest_hash: slot.manifest.manifest_hash.clone(),
            verification_generation,
        };
        state.handles.insert(
            handle_id,
            HandleBinding {
                build_content_hash: build_content_hash.to_owned(),
                root_identity_hash: slot.root_identity_hash.clone(),
                manifest_hash: slot.manifest.manifest_hash.clone(),
                verification_generation,
            },
        );
        Ok(StoredSlotObservationV1 {
            handle,
            manifest: slot.manifest,
            root_identity_hash: slot.root_identity_hash,
            owner_identity_hash: self.owner_identity_hash.clone(),
            volume_identity_hash: self.volume_identity_hash.clone(),
            per_user_owned: state.guarantees.per_user_owned,
            same_volume_as_managed_root: state.guarantees.same_volume,
            immutable: state.guarantees.immutable,
            no_follow_anchored: state.guarantees.no_follow,
        })
    }

    fn validate_handle<'a>(
        state: &'a State,
        handle: &OpenBuildSlotHandleV1,
    ) -> Result<&'a HandleBinding, BuildSlotError> {
        let binding = state
            .handles
            .get(&handle.handle_id)
            .ok_or(BuildSlotError::IdentityChanged)?;
        if binding.build_content_hash != handle.build_content_hash
            || binding.root_identity_hash != handle.root_identity_hash
            || binding.manifest_hash != handle.manifest_hash
            || binding.verification_generation != handle.verification_generation
        {
            return Err(BuildSlotError::IdentityChanged);
        }
        Ok(binding)
    }
}

impl BuildSlotStoragePortV1 for InMemoryBuildSlotStorage {
    fn begin_materialization(
        &self,
        build_content_hash: &str,
    ) -> Result<SlotMaterializationV1, BuildSlotError> {
        let mut state = self.state.lock().expect("slot storage lock");
        state.next_id = state.next_id.saturating_add(1);
        let id = StableId::parse(format!("slot.materialization.{}", state.next_id))
            .map_err(|_| BuildSlotError::Storage("materialization id".to_owned()))?;
        state.materializations.insert(
            id.clone(),
            Materialization {
                build_content_hash: build_content_hash.to_owned(),
                entries: BTreeMap::new(),
            },
        );
        Ok(SlotMaterializationV1 {
            materialization_id: id,
            build_content_hash: build_content_hash.to_owned(),
        })
    }

    fn write_entry_chunk(
        &self,
        materialization: &SlotMaterializationV1,
        relative_path: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), BuildSlotError> {
        let mut state = self.state.lock().expect("slot storage lock");
        let pending = state
            .materializations
            .get_mut(&materialization.materialization_id)
            .ok_or(BuildSlotError::NotFound)?;
        if pending.build_content_hash != materialization.build_content_hash {
            return Err(BuildSlotError::IdentityChanged);
        }
        let entry = pending.entries.entry(relative_path.to_owned()).or_default();
        if u64::try_from(entry.len()).map_err(|_| BuildSlotError::Bounded("entry length"))?
            != offset
        {
            return Err(BuildSlotError::Storage(
                "non-sequential entry write".to_owned(),
            ));
        }
        entry.extend_from_slice(bytes);
        Ok(())
    }

    fn publish_immutable(
        &self,
        materialization: &SlotMaterializationV1,
        manifest: &SlotManifestV1,
    ) -> Result<(), BuildSlotError> {
        let mut state = self.state.lock().expect("slot storage lock");
        let pending = state
            .materializations
            .remove(&materialization.materialization_id)
            .ok_or(BuildSlotError::NotFound)?;
        if pending.build_content_hash != manifest.build_content_hash
            || materialization.build_content_hash != manifest.build_content_hash
        {
            return Err(BuildSlotError::IdentityChanged);
        }
        for entry in &manifest.entries {
            if pending
                .entries
                .get(&entry.relative_path)
                .is_none_or(|bytes| bytes.len() as u64 != entry.byte_size)
            {
                return Err(BuildSlotError::Integrity(entry.relative_path.clone()));
            }
        }
        if pending.entries.len() != manifest.entries.len() {
            return Err(BuildSlotError::Manifest("unmanifested materialized entry"));
        }
        let root_identity_hash = format!("root:{}", manifest.build_content_hash);
        let slot = StoredSlot {
            manifest: manifest.clone(),
            entries: pending.entries,
            root_identity_hash,
        };
        match state.slots.get(&manifest.build_content_hash) {
            Some(existing)
                if existing.manifest == slot.manifest && existing.entries == slot.entries =>
            {
                Ok(())
            }
            Some(_) => Err(BuildSlotError::Integrity(
                "content-address collision".to_owned(),
            )),
            None => {
                state
                    .slots
                    .insert(manifest.build_content_hash.clone(), slot);
                Ok(())
            }
        }
    }

    fn open_anchored(
        &self,
        build_content_hash: &str,
    ) -> Result<StoredSlotObservationV1, BuildSlotError> {
        let mut state = self.state.lock().expect("slot storage lock");
        self.observation(&mut state, build_content_hash, None)
    }

    fn reobserve_anchored(
        &self,
        handle: &OpenBuildSlotHandleV1,
    ) -> Result<StoredSlotObservationV1, BuildSlotError> {
        let mut state = self.state.lock().expect("slot storage lock");
        Self::validate_handle(&state, handle)?;
        self.observation(&mut state, &handle.build_content_hash, Some(handle))
    }

    fn read_opened_entry_range(
        &self,
        handle: &OpenBuildSlotHandleV1,
        relative_path: &str,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, BuildSlotError> {
        let state = self.state.lock().expect("slot storage lock");
        let binding = Self::validate_handle(&state, handle)?;
        let bytes = state
            .slots
            .get(&binding.build_content_hash)
            .and_then(|slot| slot.entries.get(relative_path))
            .ok_or(BuildSlotError::NotFound)?;
        let start = usize::try_from(offset).map_err(|_| BuildSlotError::Bounded("entry offset"))?;
        let end = start.saturating_add(length).min(bytes.len());
        if start > bytes.len() {
            return Ok(Vec::new());
        }
        Ok(bytes[start..end].to_vec())
    }
}
