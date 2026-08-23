//! Closed manifests, observations, handles, and role descriptors for slots.

use aworkit_protocol::StableId;
use aworkit_protocol::{BuildBundleRefV1, BuildProvenanceV1};
use serde::{Deserialize, Serialize};

/// First immutable-slot schema.
pub const SLOT_SCHEMA_VERSION_V1: u16 = 1;
/// Maximum files in one complete application bundle.
pub const MAX_SLOT_ENTRIES: usize = 16_384;
/// Maximum bytes in one bundle.
pub const MAX_SLOT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Maximum canonical relative path bytes.
pub const MAX_SLOT_PATH_BYTES: usize = 512;
/// Maximum path components below the slot root.
pub const MAX_SLOT_PATH_DEPTH: usize = 32;
/// Streaming verification and materialization range size.
pub const SLOT_READ_CHUNK_BYTES: usize = 1024 * 1024;

/// Data rollback claim sealed into the whole-bundle manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotDataCompatibilityV1 {
    RollbackCompatible,
    DeferredUntilVerified,
    ForwardOnlyMigrationRequired,
}

/// One manifest-closed file in a whole application bundle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SlotManifestEntryV1 {
    pub relative_path: String,
    pub content_hash: String,
    pub byte_size: u64,
    pub executable: bool,
    pub media_type: String,
}

/// External manifest whose digest and entry hashes define the complete bundle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SlotManifestV1 {
    pub schema_version: u16,
    pub build_content_hash: String,
    pub provenance_digest: String,
    pub target_os: String,
    pub target_arch: String,
    pub helper_protocol_min: u16,
    pub helper_protocol_max: u16,
    pub application_schema_min: u16,
    pub application_schema_max: u16,
    pub data_compatibility: SlotDataCompatibilityV1,
    pub expected_core_entry: String,
    pub entries: Vec<SlotManifestEntryV1>,
    /// Canonical digest with this field empty. `build_content_hash` is the
    /// canonical digest with both hash fields empty, avoiding recursion.
    pub manifest_hash: String,
}

/// Fixed compatibility context owned by the helper, not the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlotCompatibilityV1 {
    pub target_os: String,
    pub target_arch: String,
    pub helper_protocol_version: u16,
    pub application_schema_version: u16,
}

/// Artifact plus decoded manifest returned by the narrow artifact-read port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedBuildArtifactV1 {
    pub bundle: BuildBundleRefV1,
    pub manifest: SlotManifestV1,
}

/// A complete staged artifact after every entry has streamed and hashed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedStagedBuildV1 {
    pub artifact_id: StableId,
    pub artifact_hash: String,
    pub build_content_hash: String,
    pub manifest_hash: String,
    pub provenance: BuildProvenanceV1,
    pub total_bytes: u64,
    pub entry_count: u32,
}

/// Opaque anchored handle. Consumers never receive a caller-supplied path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenBuildSlotHandleV1 {
    pub handle_id: StableId,
    pub build_content_hash: String,
    pub root_identity_hash: String,
    pub manifest_hash: String,
    pub verification_generation: u64,
}

/// Fresh storage facts returned for an opened, anchored slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSlotObservationV1 {
    pub handle: OpenBuildSlotHandleV1,
    pub manifest: SlotManifestV1,
    pub root_identity_hash: String,
    pub owner_identity_hash: String,
    pub volume_identity_hash: String,
    pub per_user_owned: bool,
    pub same_volume_as_managed_root: bool,
    pub immutable: bool,
    pub no_follow_anchored: bool,
}

/// Public verified slot descriptor consumed by launch and coordination ports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBuildSlotV1 {
    pub build_content_hash: String,
    pub manifest_hash: String,
    pub root_identity_hash: String,
    pub owner_identity_hash: String,
    pub volume_identity_hash: String,
    pub expected_core_entry: String,
    pub data_compatibility: SlotDataCompatibilityV1,
    pub handle: OpenBuildSlotHandleV1,
}

/// Candidate/active/previous-known-good hashes. Roles never hold paths.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ManagedSlotRolesV1 {
    pub active: Option<String>,
    pub candidate: Option<String>,
    pub previous_known_good: Option<String>,
}

/// Exact fresh observation used immediately before switch, launch, or receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlotObservationV1 {
    pub build_content_hash: String,
    pub manifest_hash: String,
    pub root_identity_hash: String,
    pub verification_generation: u64,
    pub observation_hash: String,
}

/// Storage-owned materialization identity unreachable from the active selector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlotMaterializationV1 {
    pub materialization_id: StableId,
    pub build_content_hash: String,
}
