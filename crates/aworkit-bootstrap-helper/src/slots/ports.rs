//! Narrow artifact and immutable-slot ports used by the slot manager.

use aworkit_protocol::StableId;
use aworkit_trusted_core::{BuildBundleRefV1, BuildProvenanceV1};

use super::error::BuildSlotError;
use super::model::{
    ManagedSlotRolesV1, OpenBuildSlotHandleV1, SlotManifestV1, SlotMaterializationV1,
    SlotObservationV1, StagedBuildArtifactV1, StoredSlotObservationV1, VerifiedBuildSlotV1,
    VerifiedStagedBuildV1,
};

/// Read-only artifact boundary: only ID, exact hash, manifest entry, and range.
pub trait BootstrapArtifactReadPortV1: Send + Sync {
    fn open_staged_bundle(
        &self,
        bundle: &BuildBundleRefV1,
    ) -> Result<StagedBuildArtifactV1, BuildSlotError>;

    fn read_entry_range(
        &self,
        artifact_id: &StableId,
        expected_artifact_hash: &str,
        manifest_relative_entry: &str,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, BuildSlotError>;
}

/// Native storage boundary. Implementations must anchor and no-follow handles,
/// publish complete slots atomically, and return fresh ownership/volume facts.
pub trait BuildSlotStoragePortV1: Send + Sync {
    fn begin_materialization(
        &self,
        build_content_hash: &str,
    ) -> Result<SlotMaterializationV1, BuildSlotError>;

    fn write_entry_chunk(
        &self,
        materialization: &SlotMaterializationV1,
        relative_path: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), BuildSlotError>;

    fn publish_immutable(
        &self,
        materialization: &SlotMaterializationV1,
        manifest: &SlotManifestV1,
    ) -> Result<(), BuildSlotError>;

    fn open_anchored(
        &self,
        build_content_hash: &str,
    ) -> Result<StoredSlotObservationV1, BuildSlotError>;

    fn reobserve_anchored(
        &self,
        handle: &OpenBuildSlotHandleV1,
    ) -> Result<StoredSlotObservationV1, BuildSlotError>;

    fn read_opened_entry_range(
        &self,
        handle: &OpenBuildSlotHandleV1,
        relative_path: &str,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, BuildSlotError>;
}

/// Platform-neutral whole-bundle verification and slot-role contract.
pub trait BuildSlotVerifyPortV1: Send + Sync {
    fn verify_staged_artifact(
        &self,
        bundle: &BuildBundleRefV1,
        provenance: &BuildProvenanceV1,
    ) -> Result<VerifiedStagedBuildV1, BuildSlotError>;

    fn materialize_immutable_slot(
        &self,
        bundle: &BuildBundleRefV1,
        provenance: &BuildProvenanceV1,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError>;

    fn open_verified_slot(
        &self,
        build_content_hash: &str,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError>;

    fn reverify_opened_slot(
        &self,
        handle: &OpenBuildSlotHandleV1,
    ) -> Result<VerifiedBuildSlotV1, BuildSlotError>;

    fn stage_candidate(&self, candidate: &VerifiedBuildSlotV1) -> Result<(), BuildSlotError>;
    fn set_initial_active(&self, active: &VerifiedBuildSlotV1) -> Result<(), BuildSlotError>;
    fn mark_candidate_activated_verified(&self) -> Result<(), BuildSlotError>;
    fn roles(&self) -> ManagedSlotRolesV1;
    fn produce_slot_observation(
        &self,
        handle: &OpenBuildSlotHandleV1,
    ) -> Result<SlotObservationV1, BuildSlotError>;
}
