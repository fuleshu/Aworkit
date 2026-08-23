//! Hermetic whole-bundle, TOCTOU, and slot-role tests.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use aworkit_protocol::StableId;
use aworkit_protocol::{BuildBundleRefV1, BuildProvenanceV1, RepairArtifactRefV1};
use sha2::{Digest, Sha256};

use crate::journal::canonical_hash;

use super::*;

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("valid stable id")
}

fn bytes_hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn provenance() -> BuildProvenanceV1 {
    BuildProvenanceV1 {
        source_revision: "abc123".to_owned(),
        source_tree_hash: bytes_hash(b"tree"),
        workspace_identity_hash: bytes_hash(b"workspace"),
        toolchain_hash: bytes_hash(b"toolchain"),
        build_manifest_hash: bytes_hash(b"build-manifest"),
        provenance_hash: bytes_hash(b"provenance"),
    }
}

#[derive(Clone)]
struct BundleFixture {
    bundle: BuildBundleRefV1,
    manifest: SlotManifestV1,
    entries: BTreeMap<String, Vec<u8>>,
}

fn fixture(name: &str, core_bytes: &[u8]) -> BundleFixture {
    let mut entries = BTreeMap::new();
    entries.insert("bin/aworkit-core".to_owned(), core_bytes.to_vec());
    entries.insert("resources/app.json".to_owned(), b"{\"v\":1}".to_vec());
    let manifest_entries = entries
        .iter()
        .map(|(path, bytes)| SlotManifestEntryV1 {
            relative_path: path.clone(),
            content_hash: bytes_hash(bytes),
            byte_size: bytes.len() as u64,
            executable: path.starts_with("bin/"),
            media_type: if path.starts_with("bin/") {
                "application/octet-stream".to_owned()
            } else {
                "application/json".to_owned()
            },
        })
        .collect();
    let mut manifest = SlotManifestV1 {
        schema_version: 1,
        build_content_hash: String::new(),
        provenance_digest: provenance().provenance_hash,
        target_os: "linux".to_owned(),
        target_arch: "x86_64".to_owned(),
        helper_protocol_min: 1,
        helper_protocol_max: 1,
        application_schema_min: 1,
        application_schema_max: 1,
        data_compatibility: SlotDataCompatibilityV1::RollbackCompatible,
        expected_core_entry: "bin/aworkit-core".to_owned(),
        entries: manifest_entries,
        manifest_hash: String::new(),
    };
    manifest.build_content_hash = canonical_hash(&manifest).expect("bundle root");
    manifest.manifest_hash = canonical_hash(&manifest).expect("manifest hash");
    let byte_size = entries.values().map(Vec::len).sum::<usize>() as u64;
    let bundle = BuildBundleRefV1 {
        artifact: RepairArtifactRefV1 {
            artifact_id: id(name),
            content_hash: bytes_hash(format!("archive:{name}").as_bytes()),
            byte_size,
            media_type: "application/vnd.aworkit.bundle-v1".to_owned(),
            logical_name: format!("{name}.bundle"),
        },
        manifest_relative_entry: "SlotManifest.json".to_owned(),
    };
    BundleFixture {
        bundle,
        manifest,
        entries,
    }
}

#[derive(Default)]
struct ArtifactReader {
    fixtures: Mutex<HashMap<StableId, BundleFixture>>,
}

impl ArtifactReader {
    fn insert(&self, fixture: BundleFixture) {
        self.fixtures
            .lock()
            .expect("artifact lock")
            .insert(fixture.bundle.artifact.artifact_id.clone(), fixture);
    }

    fn corrupt(&self, artifact_id: &StableId, path: &str) {
        if let Some(first) = self
            .fixtures
            .lock()
            .expect("artifact lock")
            .get_mut(artifact_id)
            .and_then(|fixture| fixture.entries.get_mut(path))
            .and_then(|bytes| bytes.first_mut())
        {
            *first ^= 0xff;
        }
    }
}

impl BootstrapArtifactReadPortV1 for ArtifactReader {
    fn open_staged_bundle(
        &self,
        bundle: &BuildBundleRefV1,
    ) -> Result<StagedBuildArtifactV1, BuildSlotError> {
        let fixture = self
            .fixtures
            .lock()
            .expect("artifact lock")
            .get(&bundle.artifact.artifact_id)
            .cloned()
            .ok_or(BuildSlotError::NotFound)?;
        if fixture.bundle != *bundle {
            return Err(BuildSlotError::Integrity("artifact binding".to_owned()));
        }
        Ok(StagedBuildArtifactV1 {
            bundle: fixture.bundle,
            manifest: fixture.manifest,
        })
    }

    fn read_entry_range(
        &self,
        artifact_id: &StableId,
        expected_artifact_hash: &str,
        manifest_relative_entry: &str,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, BuildSlotError> {
        let fixtures = self.fixtures.lock().expect("artifact lock");
        let fixture = fixtures.get(artifact_id).ok_or(BuildSlotError::NotFound)?;
        if fixture.bundle.artifact.content_hash != expected_artifact_hash {
            return Err(BuildSlotError::Integrity("artifact hash".to_owned()));
        }
        let bytes = fixture
            .entries
            .get(manifest_relative_entry)
            .ok_or(BuildSlotError::NotFound)?;
        let start = usize::try_from(offset).map_err(|_| BuildSlotError::Bounded("range"))?;
        if start > bytes.len() {
            return Ok(Vec::new());
        }
        Ok(bytes[start..start.saturating_add(length).min(bytes.len())].to_vec())
    }
}

type Harness = (
    ImmutableBuildSlotManager,
    Arc<ArtifactReader>,
    Arc<InMemoryBuildSlotStorage>,
);

fn harness(fixtures: &[BundleFixture]) -> Harness {
    let artifacts = Arc::new(ArtifactReader::default());
    for fixture in fixtures {
        artifacts.insert(fixture.clone());
    }
    let storage = Arc::new(InMemoryBuildSlotStorage::default());
    let manager = ImmutableBuildSlotManager::new(
        Arc::clone(&artifacts) as Arc<dyn BootstrapArtifactReadPortV1>,
        Arc::clone(&storage) as Arc<dyn BuildSlotStoragePortV1>,
        SlotCompatibilityV1 {
            target_os: "linux".to_owned(),
            target_arch: "x86_64".to_owned(),
            helper_protocol_version: 1,
            application_schema_version: 1,
        },
    );
    (manager, artifacts, storage)
}

#[test]
fn materializes_complete_bundle_and_revalidates_anchored_handle() {
    let fixture = fixture("artifact.1", b"core-v1");
    let (manager, _, _) = harness(std::slice::from_ref(&fixture));
    let verified = manager
        .verify_staged_artifact(&fixture.bundle, &provenance())
        .expect("verify staged");
    assert_eq!(verified.entry_count, 2);
    let slot = manager
        .materialize_immutable_slot(&fixture.bundle, &provenance())
        .expect("materialize");
    assert_eq!(slot.build_content_hash, fixture.manifest.build_content_hash);
    assert_eq!(
        manager
            .reverify_opened_slot(&slot.handle)
            .expect("reverify")
            .manifest_hash,
        slot.manifest_hash
    );
    assert_ne!(
        manager
            .produce_slot_observation(&slot.handle)
            .expect("observation 1")
            .observation_hash,
        manager
            .produce_slot_observation(&slot.handle)
            .expect("observation 2")
            .observation_hash
    );
}

#[test]
fn staged_bytes_must_match_every_manifest_entry() {
    let fixture = fixture("artifact.1", b"core-v1");
    let (manager, artifacts, _) = harness(std::slice::from_ref(&fixture));
    artifacts.corrupt(&fixture.bundle.artifact.artifact_id, "bin/aworkit-core");
    assert!(matches!(
        manager.verify_staged_artifact(&fixture.bundle, &provenance()),
        Err(BuildSlotError::Integrity(path)) if path == "bin/aworkit-core"
    ));
}

#[test]
fn manifest_rejects_traversal_case_collisions_and_helper_updates() {
    for bad_path in ["../escape", "/absolute", "bootstrap-helper"] {
        let mut fixture = fixture("artifact.1", b"core-v1");
        fixture.manifest.entries[0].relative_path = bad_path.to_owned();
        fixture.manifest.expected_core_entry = bad_path.to_owned();
        fixture.manifest.build_content_hash.clear();
        fixture.manifest.manifest_hash.clear();
        fixture.manifest.build_content_hash = canonical_hash(&fixture.manifest).expect("root");
        fixture.manifest.manifest_hash = canonical_hash(&fixture.manifest).expect("manifest");
        let (manager, _, _) = harness(std::slice::from_ref(&fixture));
        assert!(
            manager
                .verify_staged_artifact(&fixture.bundle, &provenance())
                .is_err()
        );
    }

    let mut collision = fixture("artifact.2", b"core-v1");
    let duplicate = SlotManifestEntryV1 {
        relative_path: "BIN/AWORKIT-CORE".to_owned(),
        ..collision.manifest.entries[0].clone()
    };
    collision.manifest.entries.insert(0, duplicate);
    collision.manifest.build_content_hash.clear();
    collision.manifest.manifest_hash.clear();
    collision.manifest.build_content_hash = canonical_hash(&collision.manifest).expect("root");
    collision.manifest.manifest_hash = canonical_hash(&collision.manifest).expect("manifest");
    let (manager, _, _) = harness(std::slice::from_ref(&collision));
    assert!(matches!(
        manager.verify_staged_artifact(&collision.bundle, &provenance()),
        Err(BuildSlotError::UnsafePath(_))
    ));
}

#[test]
fn last_moment_revalidation_detects_content_replacement() {
    let fixture = fixture("artifact.1", b"core-v1");
    let (manager, _, storage) = harness(std::slice::from_ref(&fixture));
    let slot = manager
        .materialize_immutable_slot(&fixture.bundle, &provenance())
        .expect("materialize");
    storage.corrupt_entry(&slot.build_content_hash, "bin/aworkit-core");
    assert!(matches!(
        manager.reverify_opened_slot(&slot.handle),
        Err(BuildSlotError::Integrity(_))
    ));
}

#[test]
fn missing_storage_guarantee_fails_before_slot_use() {
    let fixture = fixture("artifact.1", b"core-v1");
    let (manager, _, storage) = harness(std::slice::from_ref(&fixture));
    let slot = manager
        .materialize_immutable_slot(&fixture.bundle, &provenance())
        .expect("materialize");
    storage.set_guarantees(true, false, true, true);
    assert!(matches!(
        manager.reverify_opened_slot(&slot.handle),
        Err(BuildSlotError::StorageGuaranteeAbsent)
    ));
}

#[test]
fn candidate_staging_preserves_active_as_previous_until_verified() {
    let initial = fixture("artifact.initial", b"core-v1");
    let candidate = fixture("artifact.candidate", b"core-v2");
    let (manager, _, _) = harness(&[initial.clone(), candidate.clone()]);
    let active = manager
        .materialize_immutable_slot(&initial.bundle, &provenance())
        .expect("initial slot");
    let staged = manager
        .materialize_immutable_slot(&candidate.bundle, &provenance())
        .expect("candidate slot");
    manager.set_initial_active(&active).expect("set active");
    manager.stage_candidate(&staged).expect("stage candidate");
    let roles = manager.roles();
    assert_eq!(
        roles.active.as_deref(),
        Some(active.build_content_hash.as_str())
    );
    assert_eq!(
        roles.previous_known_good.as_deref(),
        Some(active.build_content_hash.as_str())
    );
    assert_eq!(
        roles.candidate.as_deref(),
        Some(staged.build_content_hash.as_str())
    );
    manager
        .mark_candidate_activated_verified()
        .expect("mark verified");
    let roles = manager.roles();
    assert_eq!(
        roles.active.as_deref(),
        Some(staged.build_content_hash.as_str())
    );
    assert_eq!(
        roles.previous_known_good.as_deref(),
        Some(active.build_content_hash.as_str())
    );
    assert!(roles.candidate.is_none());
}

#[test]
fn provenance_and_platform_must_match_exactly() {
    let mut incompatible = fixture("artifact.1", b"core-v1");
    incompatible.manifest.target_arch = "aarch64".to_owned();
    incompatible.manifest.build_content_hash.clear();
    incompatible.manifest.manifest_hash.clear();
    incompatible.manifest.build_content_hash =
        canonical_hash(&incompatible.manifest).expect("root");
    incompatible.manifest.manifest_hash = canonical_hash(&incompatible.manifest).expect("manifest");
    let (manager, _, _) = harness(std::slice::from_ref(&incompatible));
    assert!(matches!(
        manager.verify_staged_artifact(&incompatible.bundle, &provenance()),
        Err(BuildSlotError::Unsupported(_))
    ));

    let fixture = fixture("artifact.2", b"core-v1");
    let (manager, _, _) = harness(std::slice::from_ref(&fixture));
    let mut other = provenance();
    other.provenance_hash = bytes_hash(b"other");
    assert!(matches!(
        manager.verify_staged_artifact(&fixture.bundle, &other),
        Err(BuildSlotError::ProvenanceMismatch)
    ));
}
