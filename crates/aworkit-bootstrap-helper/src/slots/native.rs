//! Native immutable whole-build slot storage.

use std::{collections::HashMap, fs, path::Path, sync::Mutex};

use aworkit_process::filesystem::{AnchoredDirectory, AnchoredRelativePath};
use aworkit_protocol::StableId;

use super::{
    BuildSlotError, BuildSlotStoragePortV1, OpenBuildSlotHandleV1, SlotManifestV1,
    SlotMaterializationV1, StoredSlotObservationV1,
};

const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Default)]
struct NativeSlotState {
    next_id: u64,
    materializations: HashMap<StableId, String>,
    handles: HashMap<StableId, OpenBuildSlotHandleV1>,
}

/// Helper-controlled same-volume slot storage backed by anchored native IO.
#[derive(Debug)]
pub struct NativeBuildSlotStorage {
    root: AnchoredDirectory,
    state: Mutex<NativeSlotState>,
}

impl NativeBuildSlotStorage {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, BuildSlotError> {
        let root = AnchoredDirectory::open(root)
            .map_err(|error| BuildSlotError::Storage(error.to_string()))?;
        if !root.capability_report().supports_managed_publication() {
            return Err(BuildSlotError::StorageGuaranteeAbsent);
        }
        Ok(Self {
            root,
            state: Mutex::new(NativeSlotState::default()),
        })
    }

    fn materialization_root(
        materialization: &SlotMaterializationV1,
    ) -> Result<AnchoredRelativePath, BuildSlotError> {
        AnchoredRelativePath::parse(format!(
            ".managed/staging/{}",
            materialization.materialization_id.as_str()
        ))
        .map_err(|error| BuildSlotError::Storage(error.to_string()))
    }

    fn staging_entry(
        materialization: &SlotMaterializationV1,
        relative_path: &str,
    ) -> Result<AnchoredRelativePath, BuildSlotError> {
        AnchoredRelativePath::parse(format!(
            "{}/{}",
            Self::materialization_root(materialization)?
                .as_path()
                .to_string_lossy(),
            relative_path
        ))
        .map_err(|error| BuildSlotError::UnsafePath(error.to_string()))
    }

    fn slot_root(build_hash: &str) -> Result<AnchoredRelativePath, BuildSlotError> {
        let digest = build_hash
            .strip_prefix("sha256:")
            .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or(BuildSlotError::Manifest("build content hash"))?;
        AnchoredRelativePath::parse(format!(".managed/slots/{digest}"))
            .map_err(|error| BuildSlotError::Storage(error.to_string()))
    }

    fn slot_entry(
        build_hash: &str,
        relative_path: &str,
    ) -> Result<AnchoredRelativePath, BuildSlotError> {
        AnchoredRelativePath::parse(format!(
            "{}/{}",
            Self::slot_root(build_hash)?.as_path().to_string_lossy(),
            relative_path
        ))
        .map_err(|error| BuildSlotError::UnsafePath(error.to_string()))
    }

    fn manifest_path(build_hash: &str) -> Result<AnchoredRelativePath, BuildSlotError> {
        Self::slot_entry(build_hash, ".aworkit-slot-manifest.json")
    }

    fn observe(
        &self,
        build_content_hash: &str,
        existing: Option<&OpenBuildSlotHandleV1>,
    ) -> Result<StoredSlotObservationV1, BuildSlotError> {
        let bytes = self
            .root
            .read_bounded(&Self::manifest_path(build_content_hash)?, MAX_MANIFEST_BYTES)
            .map_err(|error| {
                if matches!(error, aworkit_process::filesystem::NativeFilesystemError::Io(ref io) if io.kind() == std::io::ErrorKind::NotFound)
                {
                    BuildSlotError::NotFound
                } else {
                    BuildSlotError::Storage(error.to_string())
                }
            })?;
        let manifest: SlotManifestV1 = serde_json::from_slice(&bytes)
            .map_err(|_| BuildSlotError::Manifest("stored manifest JSON"))?;
        if manifest.build_content_hash != build_content_hash {
            return Err(BuildSlotError::IdentityChanged);
        }
        let slot_root = Self::slot_root(build_content_hash)?;
        let identity = self
            .root
            .identify_existing(&slot_root)
            .map_err(|error| BuildSlotError::Storage(error.to_string()))?;
        let mut state = self.state.lock().expect("native slot state lock");
        state.next_id = state.next_id.saturating_add(1);
        let verification_generation = existing
            .map(|handle| handle.verification_generation)
            .unwrap_or(state.next_id);
        let handle_id = existing.map_or_else(
            || {
                StableId::parse(format!("slot.native.handle.{}", state.next_id))
                    .expect("generated native handle")
            },
            |handle| handle.handle_id.clone(),
        );
        let handle = OpenBuildSlotHandleV1 {
            handle_id: handle_id.clone(),
            build_content_hash: build_content_hash.to_owned(),
            root_identity_hash: identity.object_identity.clone(),
            manifest_hash: manifest.manifest_hash.clone(),
            verification_generation,
        };
        if let Some(expected) = existing
            && expected != &handle
        {
            return Err(BuildSlotError::IdentityChanged);
        }
        state.handles.insert(handle_id, handle.clone());
        let capabilities = self.root.capability_report();
        Ok(StoredSlotObservationV1 {
            handle,
            manifest,
            root_identity_hash: identity.object_identity,
            owner_identity_hash: self.root.identity().object_identity.clone(),
            volume_identity_hash: identity.volume_identity.clone(),
            per_user_owned: capabilities.ownership_observed,
            same_volume_as_managed_root: identity.volume_identity
                == self.root.identity().volume_identity,
            immutable: tree_is_read_only(&self.root.root().join(slot_root.as_path()))?,
            no_follow_anchored: capabilities.no_follow_components,
        })
    }

    fn validate_handle(&self, handle: &OpenBuildSlotHandleV1) -> Result<(), BuildSlotError> {
        let state = self.state.lock().expect("native slot state lock");
        if state.handles.get(&handle.handle_id) == Some(handle) {
            Ok(())
        } else {
            Err(BuildSlotError::IdentityChanged)
        }
    }
}

impl BuildSlotStoragePortV1 for NativeBuildSlotStorage {
    fn begin_materialization(
        &self,
        build_content_hash: &str,
    ) -> Result<SlotMaterializationV1, BuildSlotError> {
        Self::slot_root(build_content_hash)?;
        let mut state = self.state.lock().expect("native slot state lock");
        state.next_id = state.next_id.saturating_add(1);
        let materialization_id = StableId::parse(format!("slot.native.stage.{}", state.next_id))
            .map_err(|_| BuildSlotError::Storage("materialization identity".to_owned()))?;
        state
            .materializations
            .insert(materialization_id.clone(), build_content_hash.to_owned());
        Ok(SlotMaterializationV1 {
            materialization_id,
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
        if self
            .state
            .lock()
            .expect("native slot state lock")
            .materializations
            .get(&materialization.materialization_id)
            != Some(&materialization.build_content_hash)
        {
            return Err(BuildSlotError::IdentityChanged);
        }
        self.root
            .append_staged_chunk(
                &Self::staging_entry(materialization, relative_path)?,
                offset,
                bytes,
            )
            .map_err(|error| BuildSlotError::Storage(error.to_string()))
    }

    fn publish_immutable(
        &self,
        materialization: &SlotMaterializationV1,
        manifest: &SlotManifestV1,
    ) -> Result<(), BuildSlotError> {
        let mut state = self.state.lock().expect("native slot state lock");
        if state
            .materializations
            .get(&materialization.materialization_id)
            != Some(&materialization.build_content_hash)
            || materialization.build_content_hash != manifest.build_content_hash
        {
            return Err(BuildSlotError::IdentityChanged);
        }
        let manifest_relative =
            Self::staging_entry(materialization, ".aworkit-slot-manifest.json")?;
        let manifest_bytes = serde_json::to_vec(manifest)
            .map_err(|_| BuildSlotError::Manifest("stored manifest JSON"))?;
        self.root
            .create_new_durable(&manifest_relative, &manifest_bytes)
            .map_err(|error| BuildSlotError::Storage(error.to_string()))?;
        let staging_root = Self::materialization_root(materialization)?;
        seal_tree(&self.root.root().join(staging_root.as_path()), manifest)?;
        self.root
            .publish_directory(
                &staging_root,
                &Self::slot_root(&manifest.build_content_hash)?,
            )
            .map_err(|error| BuildSlotError::Storage(error.to_string()))?;
        let published_root = Self::slot_root(&manifest.build_content_hash)?;
        set_read_only(&self.root.root().join(published_root.as_path()), true)?;
        self.root
            .sync_existing_directory(&published_root)
            .map_err(|error| BuildSlotError::Storage(error.to_string()))?;
        state
            .materializations
            .remove(&materialization.materialization_id);
        Ok(())
    }

    fn open_anchored(
        &self,
        build_content_hash: &str,
    ) -> Result<StoredSlotObservationV1, BuildSlotError> {
        self.observe(build_content_hash, None)
    }

    fn reobserve_anchored(
        &self,
        handle: &OpenBuildSlotHandleV1,
    ) -> Result<StoredSlotObservationV1, BuildSlotError> {
        self.validate_handle(handle)?;
        self.observe(&handle.build_content_hash, Some(handle))
    }

    fn read_opened_entry_range(
        &self,
        handle: &OpenBuildSlotHandleV1,
        relative_path: &str,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, BuildSlotError> {
        self.validate_handle(handle)?;
        let bytes = self
            .root
            .read_bounded(
                &Self::slot_entry(&handle.build_content_hash, relative_path)?,
                usize::try_from(super::MAX_SLOT_BYTES)
                    .map_err(|_| BuildSlotError::Bounded("slot byte count"))?,
            )
            .map_err(|error| BuildSlotError::Storage(error.to_string()))?;
        let start = usize::try_from(offset).map_err(|_| BuildSlotError::Bounded("entry offset"))?;
        if start > bytes.len() {
            return Ok(Vec::new());
        }
        Ok(bytes[start..start.saturating_add(length).min(bytes.len())].to_vec())
    }
}

fn seal_tree(path: &Path, manifest: &SlotManifestV1) -> Result<(), BuildSlotError> {
    for entry in &manifest.entries {
        let target = path.join(&entry.relative_path);
        let metadata = fs::symlink_metadata(&target)
            .map_err(|error| BuildSlotError::Storage(error.to_string()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(BuildSlotError::UnsafePath(entry.relative_path.clone()));
        }
        set_read_only(&target, entry.executable)?;
    }
    set_read_only(&path.join(".aworkit-slot-manifest.json"), false)?;
    seal_directories(path)
}

fn seal_directories(path: &Path) -> Result<(), BuildSlotError> {
    for entry in fs::read_dir(path).map_err(|error| BuildSlotError::Storage(error.to_string()))? {
        let entry = entry.map_err(|error| BuildSlotError::Storage(error.to_string()))?;
        if entry
            .file_type()
            .map_err(|error| BuildSlotError::Storage(error.to_string()))?
            .is_dir()
        {
            seal_directories(&entry.path())?;
            set_read_only(&entry.path(), true)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_read_only(path: &Path, executable: bool) -> Result<(), BuildSlotError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if path.is_dir() || executable {
        0o555
    } else {
        0o444
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| BuildSlotError::Storage(error.to_string()))
}

#[cfg(not(unix))]
fn set_read_only(path: &Path, _executable: bool) -> Result<(), BuildSlotError> {
    let mut permissions = fs::metadata(path)
        .map_err(|error| BuildSlotError::Storage(error.to_string()))?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)
        .map_err(|error| BuildSlotError::Storage(error.to_string()))
}

fn tree_is_read_only(path: &Path) -> Result<bool, BuildSlotError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| BuildSlotError::Storage(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.permissions().readonly() {
        return Ok(false);
    }
    if metadata.is_dir() {
        for entry in
            fs::read_dir(path).map_err(|error| BuildSlotError::Storage(error.to_string()))?
        {
            let entry = entry.map_err(|error| BuildSlotError::Storage(error.to_string()))?;
            if !tree_is_read_only(&entry.path())? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots::{SlotDataCompatibilityV1, SlotManifestEntryV1};

    #[test]
    fn native_storage_publishes_one_immutable_same_volume_slot() {
        let temporary = tempfile::tempdir().expect("managed root");
        let storage = NativeBuildSlotStorage::open(temporary.path()).expect("native storage");
        let build_hash = format!("sha256:{}", "a".repeat(64));
        let materialization = storage
            .begin_materialization(&build_hash)
            .expect("materialization");
        storage
            .write_entry_chunk(&materialization, "bin/aworkit", 0, b"binary")
            .expect("entry");
        let manifest = SlotManifestV1 {
            schema_version: 1,
            build_content_hash: build_hash.clone(),
            provenance_digest: format!("sha256:{}", "b".repeat(64)),
            target_os: std::env::consts::OS.to_owned(),
            target_arch: std::env::consts::ARCH.to_owned(),
            helper_protocol_min: 1,
            helper_protocol_max: 1,
            application_schema_min: 1,
            application_schema_max: 1,
            data_compatibility: SlotDataCompatibilityV1::RollbackCompatible,
            expected_core_entry: "bin/aworkit".to_owned(),
            entries: vec![SlotManifestEntryV1 {
                relative_path: "bin/aworkit".to_owned(),
                content_hash: format!("sha256:{}", "c".repeat(64)),
                byte_size: 6,
                executable: true,
                media_type: "application/octet-stream".to_owned(),
            }],
            manifest_hash: format!("sha256:{}", "d".repeat(64)),
        };
        storage
            .publish_immutable(&materialization, &manifest)
            .expect("publish slot");
        let observation = storage.open_anchored(&build_hash).expect("open slot");
        assert!(observation.immutable);
        assert!(observation.same_volume_as_managed_root);
        assert_eq!(
            storage
                .read_opened_entry_range(&observation.handle, "bin/aworkit", 1, 3)
                .expect("range"),
            b"ina"
        );
    }
}
