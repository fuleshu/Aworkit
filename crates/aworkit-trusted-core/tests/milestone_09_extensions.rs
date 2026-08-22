use std::sync::Arc;

use aworkit_local_store::ExtensionInventory;
use aworkit_protocol::{
    CapabilityDescriptorV1, CapabilityKindV1, CapabilitySideEffectV1, CapabilityVisibilityV1,
    ExtensionCompatibilityRangeV1, ExtensionContributionV1, ExtensionDependencyV1,
    ExtensionIdentityV1, ExtensionIntegrityStatusV1, ExtensionManifestV1, ExtensionProvenanceV1,
    ExtensionRequirementV1, ExtensionResolutionV1, HostExtensionHandshakeV1,
    InertExtensionCandidateV1, ProcessGeneration, StableId, capability_descriptor_hash_v1,
    host_extension_handshake_hash_v1,
};
use aworkit_trusted_core::{ExtensionRegistry, ExtensionRegistryError};
use tempfile::TempDir;

fn id(value: &str) -> StableId {
    StableId::parse(value).expect("stable test ID")
}

fn hash(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn descriptor(capability_id: &str) -> CapabilityDescriptorV1 {
    let mut descriptor = CapabilityDescriptorV1 {
        capability_id: id(capability_id),
        adapter_version: "1.0.0".into(),
        kind: CapabilityKindV1::Mcp,
        side_effect: CapabilitySideEffectV1::Unknown,
        guarantees_same_id_deduplication: false,
        supports_streaming: true,
        supports_cancellation: true,
        supports_continuation: false,
        supports_sessions: true,
        supports_approval_forwarding: false,
        supports_mcp_forwarding: false,
        allowed_scopes: vec!["network.configured".into()],
        secret_slots: vec!["authorization".into()],
        input_schema_hash: None,
        output_schema_hash: None,
        requires_workspace: false,
        required_isolation: None,
        maximum_concurrency: 4,
        max_input_bytes: 16 * 1024,
        max_output_bytes: 64 * 1024,
        supported_platforms: vec!["linux".into(), "macos".into(), "windows".into()],
        visibility: CapabilityVisibilityV1::Mediated,
        descriptor_hash: String::new(),
    };
    descriptor.descriptor_hash =
        capability_descriptor_hash_v1(&descriptor).expect("descriptor hash");
    descriptor
}

fn candidate(extension: &str, version: &str, content_hash: String) -> InertExtensionCandidateV1 {
    let identity = ExtensionIdentityV1 {
        extension_id: id(extension),
        version: version.into(),
        content_hash: content_hash.clone(),
    };
    InertExtensionCandidateV1 {
        manifest: ExtensionManifestV1 {
            schema_version: 1,
            identity,
            compatibility: ExtensionCompatibilityRangeV1 {
                minimum_aworkit_version: "0.1.0".into(),
                maximum_aworkit_version_exclusive: Some("1.0.0".into()),
                minimum_host_protocol: 1,
                maximum_host_protocol: 1,
            },
            entry_point_identity: "bin/example-extension".into(),
            configuration_schema_hash: None,
            contributions: vec![ExtensionContributionV1 {
                contribution_id: id(&format!("{extension}.primary")),
                descriptor: descriptor(&format!("{extension}.capability")),
            }],
            dependencies: Vec::new(),
            provenance: ExtensionProvenanceV1 {
                source: "local-fixture".into(),
                publisher: Some("fixture-publisher".into()),
                signature_status: "verified".into(),
                signature_identity: Some("fixture-key".into()),
            },
        },
        observed_content_hash: content_hash,
        observed_entry_point_identity: Some("bin/example-extension".into()),
    }
}

fn handshake(candidate: &InertExtensionCandidateV1, generation: u64) -> HostExtensionHandshakeV1 {
    let mut handshake = HostExtensionHandshakeV1 {
        host_id: id("host.primary"),
        host_generation: ProcessGeneration(generation),
        host_protocol: 1,
        identity: candidate.manifest.identity.clone(),
        entry_point_identity: candidate.manifest.entry_point_identity.clone(),
        healthy: true,
        contributions: candidate.manifest.contributions.clone(),
        handshake_hash: String::new(),
    };
    handshake.handshake_hash =
        host_extension_handshake_hash_v1(&handshake).expect("handshake hash");
    handshake
}

#[test]
fn exact_identity_requires_enablement_and_generation_attestation_before_pin() {
    let temp = TempDir::new().expect("temp");
    let inventory =
        Arc::new(ExtensionInventory::for_store_root(temp.path()).expect("extension inventory"));
    let registry = ExtensionRegistry::new(inventory.clone(), "0.1.0", 1).expect("registry");
    let candidate = candidate("extension.mcp", "1.0.0", hash('a'));
    let identity = candidate.manifest.identity.clone();
    let requirement = ExtensionRequirementV1 {
        extension_id: identity.extension_id.clone(),
        contribution_id: candidate.manifest.contributions[0].contribution_id.clone(),
        exact_version: identity.version.clone(),
        exact_content_hash: identity.content_hash.clone(),
    };

    let registered = registry
        .register_installed(id("operation.register"), candidate.clone(), None)
        .expect("register");
    assert!(!registered.enabled);
    assert!(matches!(
        registry
            .resolve_contribution(&requirement, &id("host.primary"), ProcessGeneration(7))
            .expect("resolution"),
        ExtensionResolutionV1::Disabled { .. }
    ));

    let enabled = registry
        .set_enabled(id("operation.enable"), &identity, 1, true)
        .expect("enable");
    assert_eq!(enabled.record_version, 2);
    assert!(matches!(
        registry
            .resolve_contribution(&requirement, &id("host.primary"), ProcessGeneration(7))
            .expect("resolution"),
        ExtensionResolutionV1::Unattested { .. }
    ));

    let attested = registry
        .attest_host_handshake(
            id("operation.attest"),
            &identity,
            2,
            handshake(&candidate, 7),
        )
        .expect("attest");
    assert_eq!(attested.record_version, 3);
    let resolved = registry
        .resolve_contribution(&requirement, &id("host.primary"), ProcessGeneration(7))
        .expect("resolution");
    assert!(matches!(resolved, ExtensionResolutionV1::Resolved { .. }));
    assert!(matches!(
        registry
            .resolve_contribution(&requirement, &id("host.primary"), ProcessGeneration(8))
            .expect("new generation"),
        ExtensionResolutionV1::Unattested { .. }
    ));

    let set = registry
        .materialize_attested_set(id("host.primary"), ProcessGeneration(7), &[requirement])
        .expect("attested set");
    assert_eq!(set.extensions.len(), 1);
    assert_eq!(set.host_generation, ProcessGeneration(7));
    assert!(!set.set_hash.is_empty());
    assert_eq!(inventory.audit(0, 10).expect("audit").len(), 3);
}

#[test]
fn drift_integrity_and_failed_handshake_are_preserved_without_substitution() {
    let temp = TempDir::new().expect("temp");
    let inventory =
        Arc::new(ExtensionInventory::for_store_root(temp.path()).expect("extension inventory"));
    let registry = ExtensionRegistry::new(inventory.clone(), "0.1.0", 1).expect("registry");

    let mut damaged = candidate("extension.damaged", "1.0.0", hash('b'));
    damaged.observed_content_hash = hash('c');
    let damaged_record = registry
        .register_installed(id("operation.damaged"), damaged.clone(), None)
        .expect("preserve damaged record");
    assert_eq!(
        damaged_record.integrity,
        ExtensionIntegrityStatusV1::ContentHashMismatch
    );
    assert!(damaged_record.quarantine.is_some());
    assert!(matches!(
        registry.set_enabled(
            id("operation.enable.damaged"),
            damaged_record.identity(),
            1,
            true,
        ),
        Err(ExtensionRegistryError::IntegrityUnavailable)
    ));

    let valid = candidate("extension.valid", "1.0.0", hash('d'));
    let identity = valid.manifest.identity.clone();
    registry
        .register_installed(id("operation.valid"), valid.clone(), None)
        .expect("register valid");
    registry
        .set_enabled(id("operation.enable.valid"), &identity, 1, true)
        .expect("enable valid");
    let mut drifted_handshake = handshake(&valid, 9);
    drifted_handshake.entry_point_identity = "bin/substituted".into();
    drifted_handshake.handshake_hash =
        host_extension_handshake_hash_v1(&drifted_handshake).expect("drift hash");
    assert!(matches!(
        registry.attest_host_handshake(
            id("operation.reject.handshake"),
            &identity,
            2,
            drifted_handshake,
        ),
        Err(ExtensionRegistryError::AttestationRejected(_))
    ));
    let persisted = inventory
        .load_record(&identity)
        .expect("load")
        .expect("record");
    assert_eq!(persisted.record_version, 3);
    assert_eq!(
        persisted
            .quarantine
            .as_ref()
            .map(|value| value.code.as_str()),
        Some("attestation_failed")
    );

    let drift_requirement = ExtensionRequirementV1 {
        extension_id: identity.extension_id.clone(),
        contribution_id: valid.manifest.contributions[0].contribution_id.clone(),
        exact_version: "2.0.0".into(),
        exact_content_hash: hash('e'),
    };
    let resolution = registry
        .resolve_contribution(
            &drift_requirement,
            &id("host.primary"),
            ProcessGeneration(9),
        )
        .expect("drift resolution");
    assert!(matches!(resolution, ExtensionResolutionV1::Drifted { .. }));
    assert_eq!(inventory.list_records(None).expect("records").len(), 2);
    assert_eq!(inventory.audit(0, 20).expect("audit").len(), 4);
}

#[test]
fn dependency_transitions_invalidate_resolution_until_both_extensions_are_reattested() {
    let temp = TempDir::new().expect("temp");
    let inventory =
        Arc::new(ExtensionInventory::for_store_root(temp.path()).expect("extension inventory"));
    let registry = ExtensionRegistry::new(inventory, "0.1.0", 1).expect("registry");
    let dependency = candidate("extension.dependency", "1.0.0", hash('f'));
    let dependency_identity = dependency.manifest.identity.clone();
    let mut dependent = candidate("extension.dependent", "1.0.0", hash('0'));
    dependent.manifest.dependencies = vec![ExtensionDependencyV1 {
        extension_id: dependency_identity.extension_id.clone(),
        minimum_version: "1.0.0".into(),
        maximum_version_exclusive: Some("2.0.0".into()),
    }];
    let dependent_identity = dependent.manifest.identity.clone();
    let requirement = ExtensionRequirementV1 {
        extension_id: dependent_identity.extension_id.clone(),
        contribution_id: dependent.manifest.contributions[0].contribution_id.clone(),
        exact_version: dependent_identity.version.clone(),
        exact_content_hash: dependent_identity.content_hash.clone(),
    };
    let host_id = id("host.primary");
    let generation = ProcessGeneration(11);

    registry
        .register_installed(
            id("operation.dependency.register"),
            dependency.clone(),
            None,
        )
        .expect("register dependency");
    registry
        .set_enabled(
            id("operation.dependency.enable"),
            &dependency_identity,
            1,
            true,
        )
        .expect("enable dependency");
    registry
        .attest_host_handshake(
            id("operation.dependency.attest"),
            &dependency_identity,
            2,
            handshake(&dependency, generation.0),
        )
        .expect("attest dependency");
    registry
        .register_installed(id("operation.dependent.register"), dependent.clone(), None)
        .expect("register dependent");
    registry
        .set_enabled(
            id("operation.dependent.enable"),
            &dependent_identity,
            1,
            true,
        )
        .expect("enable dependent");
    registry
        .attest_host_handshake(
            id("operation.dependent.attest"),
            &dependent_identity,
            2,
            handshake(&dependent, generation.0),
        )
        .expect("attest dependent");
    assert!(matches!(
        registry
            .resolve_contribution(&requirement, &host_id, generation)
            .expect("initial resolution"),
        ExtensionResolutionV1::Resolved { .. }
    ));
    registry
        .materialize_attested_set(
            host_id.clone(),
            generation,
            std::slice::from_ref(&requirement),
        )
        .expect("initial materialization");

    let disabled = registry
        .set_enabled(
            id("operation.dependency.disable"),
            &dependency_identity,
            3,
            false,
        )
        .expect("disable dependency");
    assert!(disabled.last_attestation.is_none());
    assert_dependent_unavailable(&registry, &requirement, &host_id, generation);

    let enabled = registry
        .set_enabled(
            id("operation.dependency.reenable"),
            &dependency_identity,
            4,
            true,
        )
        .expect("re-enable dependency");
    assert!(enabled.last_attestation.is_none());
    registry
        .attest_host_handshake(
            id("operation.dependency.reattest.after-disable"),
            &dependency_identity,
            5,
            handshake(&dependency, generation.0),
        )
        .expect("re-attest dependency after disable");
    assert_dependent_unavailable(&registry, &requirement, &host_id, generation);
    registry
        .attest_host_handshake(
            id("operation.dependent.reattest.after-disable"),
            &dependent_identity,
            3,
            handshake(&dependent, generation.0),
        )
        .expect("re-attest dependent after dependency disable");
    assert_dependent_resolved(&registry, &requirement, &host_id, generation);

    let quarantined = registry
        .quarantine(
            id("operation.dependency.quarantine"),
            &dependency_identity,
            6,
            "test_quarantine",
            "dependency quarantined by regression test",
        )
        .expect("quarantine dependency");
    assert!(quarantined.last_attestation.is_none());
    assert_dependent_unavailable(&registry, &requirement, &host_id, generation);

    let recovered = registry
        .register_installed(
            id("operation.dependency.recover"),
            dependency.clone(),
            Some(7),
        )
        .expect("recover dependency registration");
    assert!(recovered.enabled);
    assert!(recovered.last_attestation.is_none());
    registry
        .attest_host_handshake(
            id("operation.dependency.reattest.after-quarantine"),
            &dependency_identity,
            8,
            handshake(&dependency, generation.0),
        )
        .expect("re-attest dependency after quarantine");
    assert_dependent_unavailable(&registry, &requirement, &host_id, generation);
    registry
        .attest_host_handshake(
            id("operation.dependent.reattest.after-quarantine"),
            &dependent_identity,
            4,
            handshake(&dependent, generation.0),
        )
        .expect("re-attest dependent after dependency quarantine");
    assert_dependent_resolved(&registry, &requirement, &host_id, generation);

    let removed = registry
        .remove_installed_record(id("operation.dependency.remove"), &dependency_identity, 9)
        .expect("remove dependency");
    assert!(removed.last_attestation.is_none());
    assert_dependent_unavailable(&registry, &requirement, &host_id, generation);

    let reinstalled = registry
        .register_installed(
            id("operation.dependency.reinstall"),
            dependency.clone(),
            Some(10),
        )
        .expect("reinstall dependency");
    assert!(!reinstalled.enabled);
    registry
        .set_enabled(
            id("operation.dependency.enable.after-reinstall"),
            &dependency_identity,
            11,
            true,
        )
        .expect("enable reinstalled dependency");
    registry
        .attest_host_handshake(
            id("operation.dependency.reattest.after-remove"),
            &dependency_identity,
            12,
            handshake(&dependency, generation.0),
        )
        .expect("re-attest dependency after removal");
    assert_dependent_unavailable(&registry, &requirement, &host_id, generation);
    registry
        .attest_host_handshake(
            id("operation.dependent.reattest.after-remove"),
            &dependent_identity,
            5,
            handshake(&dependent, generation.0),
        )
        .expect("re-attest dependent after dependency removal");
    assert_dependent_resolved(&registry, &requirement, &host_id, generation);
}

fn assert_dependent_unavailable(
    registry: &ExtensionRegistry,
    requirement: &ExtensionRequirementV1,
    host_id: &StableId,
    generation: ProcessGeneration,
) {
    assert!(matches!(
        registry
            .resolve_contribution(requirement, host_id, generation)
            .expect("dependency resolution"),
        ExtensionResolutionV1::Unattested { .. }
    ));
    assert!(matches!(
        registry.materialize_attested_set(
            host_id.clone(),
            generation,
            std::slice::from_ref(requirement),
        ),
        Err(ExtensionRegistryError::RequirementUnavailable(_))
    ));
}

fn assert_dependent_resolved(
    registry: &ExtensionRegistry,
    requirement: &ExtensionRequirementV1,
    host_id: &StableId,
    generation: ProcessGeneration,
) {
    assert!(matches!(
        registry
            .resolve_contribution(requirement, host_id, generation)
            .expect("dependency resolution"),
        ExtensionResolutionV1::Resolved { .. }
    ));
    registry
        .materialize_attested_set(
            host_id.clone(),
            generation,
            std::slice::from_ref(requirement),
        )
        .expect("dependent materialization");
}
