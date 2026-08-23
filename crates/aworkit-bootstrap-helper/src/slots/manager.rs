//! Streaming verifier, immutable materializer, and slot-role state machine.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use aworkit_protocol::StableId;
use aworkit_trusted_core::{BuildBundleRefV1, BuildProvenanceV1};
use sha2::{Digest, Sha256};

use crate::journal::canonical_hash;

use super::error::BuildSlotError;
use super::model::*;
use super::ports::{BootstrapArtifactReadPortV1, BuildSlotStoragePortV1, BuildSlotVerifyPortV1};

/// Platform-neutral whole-bundle verifier and immutable role manager.
pub struct ImmutableBuildSlotManager {
    artifacts: Arc<dyn BootstrapArtifactReadPortV1>,
    storage: Arc<dyn BuildSlotStoragePortV1>,
    compatibility: SlotCompatibilityV1,
    roles: Mutex<ManagedSlotRolesV1>,
    observation_generation: AtomicU64,
}

impl ImmutableBuildSlotManager {
    #[must_use]
    pub fn new(
        artifacts: Arc<dyn BootstrapArtifactReadPortV1>,
        storage: Arc<dyn BuildSlotStoragePortV1>,
        compatibility: SlotCompatibilityV1,
    ) -> Self {
        Self {
            artifacts,
            storage,
            compatibility,
            roles: Mutex::new(ManagedSlotRolesV1::default()),
            observation_generation: AtomicU64::new(1),
        }
    }

    fn validate_manifest(
        &self,
        manifest: &SlotManifestV1,
        provenance: Option<&BuildProvenanceV1>,
    ) -> Result<u64, BuildSlotError> {
        if manifest.schema_version != SLOT_SCHEMA_VERSION_V1 {
            return Err(BuildSlotError::Unsupported("slot schema version"));
        }
        if manifest.entries.is_empty() || manifest.entries.len() > MAX_SLOT_ENTRIES {
            return Err(BuildSlotError::Bounded("slot entry count"));
        }
        if manifest.target_os != self.compatibility.target_os
            || manifest.target_arch != self.compatibility.target_arch
        {
            return Err(BuildSlotError::Unsupported("target OS or architecture"));
        }
        if !(manifest.helper_protocol_min..=manifest.helper_protocol_max)
            .contains(&self.compatibility.helper_protocol_version)
        {
            return Err(BuildSlotError::Unsupported("helper protocol range"));
        }
        if !(manifest.application_schema_min..=manifest.application_schema_max)
            .contains(&self.compatibility.application_schema_version)
        {
            return Err(BuildSlotError::Unsupported("application schema range"));
        }
        if provenance.is_some_and(|value| value.provenance_hash != manifest.provenance_digest) {
            return Err(BuildSlotError::ProvenanceMismatch);
        }

        let mut exact = BTreeSet::new();
        let mut folded = BTreeSet::new();
        let mut prior = None::<&str>;
        let mut total = 0_u64;
        let mut core_found = false;
        for entry in &manifest.entries {
            Self::validate_relative_path(&entry.relative_path)?;
            if prior.is_some_and(|value| value >= entry.relative_path.as_str()) {
                return Err(BuildSlotError::Manifest(
                    "entries must be strictly sorted by canonical path",
                ));
            }
            prior = Some(&entry.relative_path);
            if !exact.insert(entry.relative_path.clone())
                || !folded.insert(entry.relative_path.to_ascii_lowercase())
            {
                return Err(BuildSlotError::UnsafePath(entry.relative_path.clone()));
            }
            if !Self::is_sha256(&entry.content_hash) || entry.byte_size == 0 {
                return Err(BuildSlotError::Manifest("entry hash or length"));
            }
            if !Self::media_allowed(&entry.media_type, entry.executable) {
                return Err(BuildSlotError::Unsupported("entry media type"));
            }
            if Self::is_helper_owned_path(&entry.relative_path) {
                return Err(BuildSlotError::Unsupported(
                    "helper, launcher, and journal must remain outside slots",
                ));
            }
            if entry.relative_path == manifest.expected_core_entry {
                if !entry.executable {
                    return Err(BuildSlotError::Manifest("core entry is not executable"));
                }
                core_found = true;
            }
            total = total
                .checked_add(entry.byte_size)
                .ok_or(BuildSlotError::Bounded("slot byte count"))?;
            if total > MAX_SLOT_BYTES {
                return Err(BuildSlotError::Bounded("slot byte count"));
            }
        }
        Self::validate_relative_path(&manifest.expected_core_entry)?;
        if !core_found {
            return Err(BuildSlotError::Manifest("expected core entry is absent"));
        }

        let mut root = manifest.clone();
        root.build_content_hash.clear();
        root.manifest_hash.clear();
        let root_hash = canonical_hash(&root).map_err(|_| BuildSlotError::Manifest("root hash"))?;
        if manifest.build_content_hash != root_hash {
            return Err(BuildSlotError::Integrity("bundle root".to_owned()));
        }
        let mut sealed = manifest.clone();
        sealed.manifest_hash.clear();
        let manifest_hash =
            canonical_hash(&sealed).map_err(|_| BuildSlotError::Manifest("manifest hash"))?;
        if manifest.manifest_hash != manifest_hash {
            return Err(BuildSlotError::Integrity("manifest".to_owned()));
        }
        Ok(total)
    }

    fn validate_relative_path(path: &str) -> Result<(), BuildSlotError> {
        let parts = path.split('/').collect::<Vec<_>>();
        let valid = !path.is_empty()
            && path.len() <= MAX_SLOT_PATH_BYTES
            && path.is_ascii()
            && !path.starts_with('/')
            && !path.contains('\\')
            && !path.contains(':')
            && !path.contains('\0')
            && parts.len() <= MAX_SLOT_PATH_DEPTH
            && parts
                .iter()
                .all(|part| !part.is_empty() && *part != "." && *part != "..");
        if valid {
            Ok(())
        } else {
            Err(BuildSlotError::UnsafePath(path.to_owned()))
        }
    }

    fn is_helper_owned_path(path: &str) -> bool {
        let lower = path.to_ascii_lowercase();
        lower.contains("bootstrap-helper")
            || lower.contains("stable-launcher")
            || lower.starts_with("journal/")
    }

    fn media_allowed(media: &str, executable: bool) -> bool {
        if executable {
            return media == "application/octet-stream";
        }
        matches!(
            media,
            "application/octet-stream"
                | "application/json"
                | "application/javascript"
                | "text/plain"
                | "text/css"
                | "image/png"
                | "image/jpeg"
                | "image/svg+xml"
        )
    }

    fn is_sha256(value: &str) -> bool {
        value.len() == 71
            && value.starts_with("sha256:")
            && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    fn verify_artifact_entry(
        &self,
        artifact_id: &StableId,
        artifact_hash: &str,
        entry: &SlotManifestEntryV1,
        materialization: Option<&SlotMaterializationV1>,
    ) -> Result<(), BuildSlotError> {
        let mut hasher = Sha256::new();
        let mut offset = 0_u64;
        while offset < entry.byte_size {
            let remaining = entry.byte_size - offset;
            let length = usize::try_from(remaining.min(SLOT_READ_CHUNK_BYTES as u64))
                .map_err(|_| BuildSlotError::Bounded("entry range"))?;
            let bytes = self.artifacts.read_entry_range(
                artifact_id,
                artifact_hash,
                &entry.relative_path,
                offset,
                length,
            )?;
            if bytes.len() != length {
                return Err(BuildSlotError::Integrity(entry.relative_path.clone()));
            }
            hasher.update(&bytes);
            if let Some(materialization) = materialization {
                self.storage.write_entry_chunk(
                    materialization,
                    &entry.relative_path,
                    offset,
                    &bytes,
                )?;
            }
            offset +=
                u64::try_from(bytes.len()).map_err(|_| BuildSlotError::Bounded("entry length"))?;
        }
        let actual = format!("sha256:{:x}", hasher.finalize());
        if actual != entry.content_hash {
            return Err(BuildSlotError::Integrity(entry.relative_path.clone()));
        }
        Ok(())
    }

    fn verify_opened_entry(
        &self,
        handle: &OpenBuildSlotHandleV1,
        entry: &SlotManifestEntryV1,
    ) -> Result<(), BuildSlotError> {
        let mut hasher = Sha256::new();
        let mut offset = 0_u64;
        while offset < entry.byte_size {
            let remaining = entry.byte_size - offset;
            let length = usize::try_from(remaining.min(SLOT_READ_CHUNK_BYTES as u64))
                .map_err(|_| BuildSlotError::Bounded("entry range"))?;
            let bytes = self.storage.read_opened_entry_range(
                handle,
                &entry.relative_path,
                offset,
                length,
            )?;
            if bytes.len() != length {
                return Err(BuildSlotError::Integrity(entry.relative_path.clone()));
            }
            hasher.update(&bytes);
            offset +=
                u64::try_from(bytes.len()).map_err(|_| BuildSlotError::Bounded("entry length"))?;
        }
        if format!("sha256:{:x}", hasher.finalize()) != entry.content_hash {
            return Err(BuildSlotError::Integrity(entry.relative_path.clone()));
        }
        Ok(())
    }

    fn verified_from_observation(
        &self,
        observation: StoredSlotObservationV1,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        self.validate_manifest(&observation.manifest, None)?;
        if observation.manifest.build_content_hash != observation.handle.build_content_hash {
            return Err(BuildSlotError::IdentityChanged);
        }
        if !observation.per_user_owned
            || !observation.same_volume_as_managed_root
            || !observation.immutable
            || !observation.no_follow_anchored
        {
            return Err(BuildSlotError::StorageGuaranteeAbsent);
        }
        if observation.handle.root_identity_hash != observation.root_identity_hash
            || observation.handle.manifest_hash != observation.manifest.manifest_hash
            || observation.root_identity_hash.is_empty()
            || observation.owner_identity_hash.is_empty()
            || observation.volume_identity_hash.is_empty()
        {
            return Err(BuildSlotError::IdentityChanged);
        }
        for entry in &observation.manifest.entries {
            self.verify_opened_entry(&observation.handle, entry)?;
        }
        Ok(VerifiedBuildSlotV1 {
            build_content_hash: observation.manifest.build_content_hash,
            manifest_hash: observation.manifest.manifest_hash,
            root_identity_hash: observation.root_identity_hash,
            owner_identity_hash: observation.owner_identity_hash,
            volume_identity_hash: observation.volume_identity_hash,
            expected_core_entry: observation.manifest.expected_core_entry,
            data_compatibility: observation.manifest.data_compatibility,
            handle: observation.handle,
        })
    }
}

impl BuildSlotVerifyPortV1 for ImmutableBuildSlotManager {
    fn verify_staged_artifact(
        &self,
        bundle: &BuildBundleRefV1,
        provenance: &BuildProvenanceV1,
    ) -> Result<VerifiedStagedBuildV1, BuildSlotError> {
        if bundle.artifact.byte_size == 0 || bundle.artifact.byte_size > MAX_SLOT_BYTES {
            return Err(BuildSlotError::Bounded("artifact byte count"));
        }
        Self::validate_relative_path(&bundle.manifest_relative_entry)?;
        let staged = self.artifacts.open_staged_bundle(bundle)?;
        if staged.bundle != *bundle {
            return Err(BuildSlotError::Integrity("artifact descriptor".to_owned()));
        }
        let total = self.validate_manifest(&staged.manifest, Some(provenance))?;
        for entry in &staged.manifest.entries {
            self.verify_artifact_entry(
                &bundle.artifact.artifact_id,
                &bundle.artifact.content_hash,
                entry,
                None,
            )?;
        }
        Ok(VerifiedStagedBuildV1 {
            artifact_id: bundle.artifact.artifact_id.clone(),
            artifact_hash: bundle.artifact.content_hash.clone(),
            build_content_hash: staged.manifest.build_content_hash,
            manifest_hash: staged.manifest.manifest_hash,
            provenance: provenance.clone(),
            total_bytes: total,
            entry_count: u32::try_from(staged.manifest.entries.len())
                .map_err(|_| BuildSlotError::Bounded("entry count"))?,
        })
    }

    fn materialize_immutable_slot(
        &self,
        bundle: &BuildBundleRefV1,
        provenance: &BuildProvenanceV1,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        let verified = self.verify_staged_artifact(bundle, provenance)?;
        match self.open_verified_slot(&verified.build_content_hash) {
            Ok(existing) => return Ok(existing),
            Err(BuildSlotError::NotFound) => {}
            Err(error) => return Err(error),
        }
        let staged = self.artifacts.open_staged_bundle(bundle)?;
        let materialization = self
            .storage
            .begin_materialization(&verified.build_content_hash)?;
        for entry in &staged.manifest.entries {
            self.verify_artifact_entry(
                &bundle.artifact.artifact_id,
                &bundle.artifact.content_hash,
                entry,
                Some(&materialization),
            )?;
        }
        self.storage
            .publish_immutable(&materialization, &staged.manifest)?;
        self.open_verified_slot(&verified.build_content_hash)
    }

    fn open_verified_slot(
        &self,
        build_content_hash: &str,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        if !Self::is_sha256(build_content_hash) {
            return Err(BuildSlotError::Manifest("build content hash"));
        }
        self.verified_from_observation(self.storage.open_anchored(build_content_hash)?)
    }

    fn reverify_opened_slot(
        &self,
        handle: &OpenBuildSlotHandleV1,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError> {
        let observation = self.storage.reobserve_anchored(handle)?;
        if observation.handle.build_content_hash != handle.build_content_hash
            || observation.root_identity_hash != handle.root_identity_hash
            || observation.manifest.manifest_hash != handle.manifest_hash
        {
            return Err(BuildSlotError::IdentityChanged);
        }
        self.verified_from_observation(observation)
    }

    fn stage_candidate(&self, candidate: &VerifiedBuildSlotV1) -> Result<(), BuildSlotError> {
        self.reverify_opened_slot(&candidate.handle)?;
        let mut roles = self.roles.lock().expect("slot-role lock");
        let active = roles
            .active
            .clone()
            .ok_or(BuildSlotError::IllegalRoleTransition)?;
        if active == candidate.build_content_hash {
            return Err(BuildSlotError::IllegalRoleTransition);
        }
        roles.previous_known_good = Some(active);
        roles.candidate = Some(candidate.build_content_hash.clone());
        Ok(())
    }

    fn set_initial_active(&self, active: &VerifiedBuildSlotV1) -> Result<(), BuildSlotError> {
        self.reverify_opened_slot(&active.handle)?;
        let mut roles = self.roles.lock().expect("slot-role lock");
        if roles.active.is_some()
            || roles.candidate.is_some()
            || roles.previous_known_good.is_some()
        {
            return Err(BuildSlotError::IllegalRoleTransition);
        }
        roles.active = Some(active.build_content_hash.clone());
        Ok(())
    }

    fn mark_candidate_activated_verified(&self) -> Result<(), BuildSlotError> {
        let mut roles = self.roles.lock().expect("slot-role lock");
        let candidate = roles
            .candidate
            .take()
            .ok_or(BuildSlotError::IllegalRoleTransition)?;
        roles.active = Some(candidate);
        Ok(())
    }

    fn roles(&self) -> ManagedSlotRolesV1 {
        self.roles.lock().expect("slot-role lock").clone()
    }

    fn produce_slot_observation(
        &self,
        handle: &OpenBuildSlotHandleV1,
    ) -> Result<SlotObservationV1, BuildSlotError> {
        let verified = self.reverify_opened_slot(handle)?;
        let verification_generation = self.observation_generation.fetch_add(1, Ordering::SeqCst);
        let observation_hash = canonical_hash(&(
            &verified.build_content_hash,
            &verified.manifest_hash,
            &verified.root_identity_hash,
            verification_generation,
        ))
        .map_err(|_| BuildSlotError::Manifest("observation hash"))?;
        Ok(SlotObservationV1 {
            build_content_hash: verified.build_content_hash,
            manifest_hash: verified.manifest_hash,
            root_identity_hash: verified.root_identity_hash,
            verification_generation,
            observation_hash,
        })
    }
}
