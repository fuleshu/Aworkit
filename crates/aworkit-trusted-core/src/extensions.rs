//! Core-owned trusted-extension identity, compatibility, and attestation.
//!
//! Discovery inputs are metadata-only. This module never loads an entry point
//! or starts extension code; it governs facts produced by bounded adapters.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use aworkit_protocol::{
    AttestedExtensionSetV1, AttestedExtensionV1, CapabilityDescriptorV1,
    EXTENSION_MANIFEST_SCHEMA_V1, ExtensionAuditKindV1, ExtensionCompatibilityStatusV1,
    ExtensionDependencyV1, ExtensionIdentityV1, ExtensionIntegrityStatusV1, ExtensionInventoryPort,
    ExtensionInventoryPortErrorV1, ExtensionInventoryWriteV1, ExtensionManifestV1,
    ExtensionProtocolError, ExtensionQuarantineV1, ExtensionRecordV1, ExtensionRequirementV1,
    ExtensionResolutionV1, HostExtensionAttestationV1, HostExtensionHandshakeV1,
    InertExtensionCandidateV1, PinnedExtensionContributionV1, ProcessGeneration, StableId,
    attested_extension_set_hash_v1, capability_descriptor_hash_v1,
    host_extension_handshake_hash_v1, is_canonical_sha256,
};
use semver::Version;
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

const MAX_MANIFEST_CONTRIBUTIONS: usize = 256;
const MAX_MANIFEST_DEPENDENCIES: usize = 256;

/// Trusted registry over a persistence-only inventory port.
pub struct ExtensionRegistry {
    inventory: Arc<dyn ExtensionInventoryPort>,
    aworkit_version: Version,
    aworkit_version_text: String,
    host_protocol: u16,
}

impl ExtensionRegistry {
    /// Creates a registry for one exact application build and host protocol.
    pub fn new(
        inventory: Arc<dyn ExtensionInventoryPort>,
        aworkit_version: &str,
        host_protocol: u16,
    ) -> Result<Self, ExtensionRegistryError> {
        if host_protocol == 0 {
            return Err(ExtensionRegistryError::InvalidHostProtocol);
        }
        let parsed = Version::parse(aworkit_version)
            .map_err(|_| ExtensionRegistryError::InvalidAworkitVersion)?;
        Ok(Self {
            inventory,
            aworkit_version: parsed,
            aworkit_version_text: aworkit_version.to_owned(),
            host_protocol,
        })
    }

    /// Registers inert metadata for one exact package identity. Installation
    /// does not imply enablement. Integrity or compatibility failures are
    /// retained as quarantined, inspectable records.
    pub fn register_installed(
        &self,
        operation_id: StableId,
        mut candidate: InertExtensionCandidateV1,
        expected_version: Option<u64>,
    ) -> Result<ExtensionRecordV1, ExtensionRegistryError> {
        normalize_manifest(&mut candidate.manifest);
        validate_identity_key(&candidate.manifest)?;
        let existing = self.load(&candidate.manifest.identity)?;
        if let Some(existing) = &existing {
            if existing.manifest != candidate.manifest {
                return Err(ExtensionRegistryError::ExactIdentityMutation);
            }
        }
        let integrity = evaluate_integrity(&candidate);
        let compatibility = self.evaluate_compatibility(&candidate.manifest);
        let quarantine = registration_quarantine(&integrity, &compatibility);
        let record_version = expected_version.unwrap_or(0).saturating_add(1);
        let record = ExtensionRecordV1 {
            manifest: candidate.manifest,
            installed: true,
            enabled: existing.as_ref().is_some_and(|record| record.enabled),
            integrity,
            compatibility,
            quarantine,
            record_version,
            last_attestation: None,
        };
        self.write(ExtensionInventoryWriteV1 {
            operation_id,
            expected_version,
            record,
            audit_kind: ExtensionAuditKindV1::Registered,
            detail: "registered inert installed-extension facts".into(),
        })
    }

    /// Applies an explicit user enable/disable decision. Enablement fails closed
    /// until integrity, compatibility, quarantine, and dependencies are clear.
    pub fn set_enabled(
        &self,
        operation_id: StableId,
        identity: &ExtensionIdentityV1,
        expected_version: u64,
        enabled: bool,
    ) -> Result<ExtensionRecordV1, ExtensionRegistryError> {
        let mut record = self.require_record(identity)?;
        ensure_expected_version(&record, expected_version)?;
        if enabled {
            self.ensure_enableable(&record)?;
        }
        if record.enabled == enabled {
            return Ok(record);
        }
        record.enabled = enabled;
        record.last_attestation = None;
        record.record_version = next_version(expected_version)?;
        self.write(ExtensionInventoryWriteV1 {
            operation_id,
            expected_version: Some(expected_version),
            record,
            audit_kind: ExtensionAuditKindV1::EnablementChanged,
            detail: if enabled {
                "extension explicitly enabled"
            } else {
                "extension explicitly disabled"
            }
            .into(),
        })
    }

    /// Marks an exact identity unavailable for new dispatch without deleting
    /// its manifest, audit history, or prior attestation evidence.
    pub fn quarantine(
        &self,
        operation_id: StableId,
        identity: &ExtensionIdentityV1,
        expected_version: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<ExtensionRecordV1, ExtensionRegistryError> {
        let record = self.require_record(identity)?;
        ensure_expected_version(&record, expected_version)?;
        self.write_quarantine(operation_id, record, expected_version, code, message)
    }

    /// Retains an exact identity as historical/pinned evidence while marking it
    /// uninstalled. No row or immutable audit fact is deleted.
    pub fn remove_installed_record(
        &self,
        operation_id: StableId,
        identity: &ExtensionIdentityV1,
        expected_version: u64,
    ) -> Result<ExtensionRecordV1, ExtensionRegistryError> {
        let mut record = self.require_record(identity)?;
        ensure_expected_version(&record, expected_version)?;
        record.installed = false;
        record.enabled = false;
        record.last_attestation = None;
        record.record_version = next_version(expected_version)?;
        record.quarantine = Some(ExtensionQuarantineV1 {
            code: "removed".into(),
            message: "exact extension identity is no longer installed".into(),
        });
        self.write(ExtensionInventoryWriteV1 {
            operation_id,
            expected_version: Some(expected_version),
            record,
            audit_kind: ExtensionAuditKindV1::Removed,
            detail: "retained historical identity after explicit removal".into(),
        })
    }

    /// Re-evaluates compatibility after an application or host-protocol update.
    pub fn check_compatibility(
        &self,
        operation_id: StableId,
        identity: &ExtensionIdentityV1,
        expected_version: u64,
    ) -> Result<ExtensionRecordV1, ExtensionRegistryError> {
        let mut record = self.require_record(identity)?;
        ensure_expected_version(&record, expected_version)?;
        let compatibility = self.evaluate_compatibility(&record.manifest);
        if record.compatibility == compatibility {
            return Ok(record);
        }
        record.compatibility = compatibility;
        record.last_attestation = None;
        record.record_version = next_version(expected_version)?;
        if let ExtensionCompatibilityStatusV1::Incompatible { code, message, .. } =
            &record.compatibility
        {
            record.quarantine = Some(ExtensionQuarantineV1 {
                code: format!("compatibility_{code}"),
                message: message.clone(),
            });
        } else if record
            .quarantine
            .as_ref()
            .is_some_and(|value| value.code.starts_with("compatibility_"))
        {
            record.quarantine = None;
        }
        self.write(ExtensionInventoryWriteV1 {
            operation_id,
            expected_version: Some(expected_version),
            record,
            audit_kind: ExtensionAuditKindV1::CompatibilityEvaluated,
            detail: "re-evaluated application and host protocol compatibility".into(),
        })
    }

    /// Accepts a metadata-only handshake only when every identity, descriptor,
    /// protocol, health, and generation fence matches the enabled record.
    pub fn attest_host_handshake(
        &self,
        operation_id: StableId,
        identity: &ExtensionIdentityV1,
        expected_version: u64,
        mut handshake: HostExtensionHandshakeV1,
    ) -> Result<ExtensionRecordV1, ExtensionRegistryError> {
        let mut record = self.require_record(identity)?;
        ensure_expected_version(&record, expected_version)?;
        self.ensure_enableable(&record)?;
        normalize_contributions(&mut handshake.contributions);
        if let Some(previous) = &record.last_attestation {
            if previous.host_id == handshake.host_id
                && previous.host_generation.0 > handshake.host_generation.0
            {
                return Err(ExtensionRegistryError::StaleHostGeneration {
                    previous: previous.host_generation.0,
                    received: handshake.host_generation.0,
                });
            }
        }
        if let Err(reason) = self.validate_handshake(&record, &handshake) {
            self.write_quarantine(
                operation_id,
                record,
                expected_version,
                "attestation_failed",
                reason.clone(),
            )?;
            return Err(ExtensionRegistryError::AttestationRejected(reason));
        }
        let dependency_snapshot_hash =
            self.dependency_snapshot_hash(&record, &handshake.host_id, handshake.host_generation)?;
        if record.last_attestation.as_ref().is_some_and(|previous| {
            previous.host_id == handshake.host_id
                && previous.host_generation == handshake.host_generation
                && previous.handshake_hash == handshake.handshake_hash
                && previous.dependency_snapshot_hash == dependency_snapshot_hash
        }) {
            return Ok(record);
        }
        let descriptor_set_hash = one_extension_set_hash(&handshake)?;
        record.last_attestation = Some(HostExtensionAttestationV1 {
            host_id: handshake.host_id,
            host_generation: handshake.host_generation,
            host_protocol: handshake.host_protocol,
            handshake_hash: handshake.handshake_hash,
            descriptor_set_hash,
            dependency_snapshot_hash,
        });
        record.record_version = next_version(expected_version)?;
        self.write(ExtensionInventoryWriteV1 {
            operation_id,
            expected_version: Some(expected_version),
            record,
            audit_kind: ExtensionAuditKindV1::Attested,
            detail: "attested exact extension descriptors for supervised host generation".into(),
        })
    }

    /// Resolves one exact requirement without substituting a newer version,
    /// another content hash, or an unattested host generation.
    pub fn resolve_contribution(
        &self,
        requirement: &ExtensionRequirementV1,
        host_id: &StableId,
        host_generation: ProcessGeneration,
    ) -> Result<ExtensionResolutionV1, ExtensionRegistryError> {
        let records = self.list(Some(&requirement.extension_id))?;
        if records.is_empty() {
            return Ok(ExtensionResolutionV1::Missing);
        }
        let exact = records.iter().find(|record| {
            record.identity().version == requirement.exact_version
                && record.identity().content_hash == requirement.exact_content_hash
        });
        let Some(record) = exact else {
            return Ok(ExtensionResolutionV1::Drifted {
                expected_version: requirement.exact_version.clone(),
                expected_content_hash: requirement.exact_content_hash.clone(),
                installed_identities: records
                    .into_iter()
                    .filter(|record| record.installed)
                    .map(|record| record.manifest.identity)
                    .collect(),
            });
        };
        if !record.installed {
            return Ok(ExtensionResolutionV1::Missing);
        }
        if !record.enabled {
            return Ok(ExtensionResolutionV1::Disabled {
                identity: record.identity().clone(),
            });
        }
        if let Some(quarantine) = &record.quarantine {
            return Ok(ExtensionResolutionV1::Quarantined {
                identity: record.identity().clone(),
                code: quarantine.code.clone(),
                message: quarantine.message.clone(),
            });
        }
        if let ExtensionCompatibilityStatusV1::Incompatible { code, message, .. } =
            &record.compatibility
        {
            return Ok(ExtensionResolutionV1::Incompatible {
                identity: record.identity().clone(),
                code: code.clone(),
                message: message.clone(),
            });
        }
        if record.integrity != ExtensionIntegrityStatusV1::Verified {
            return Ok(ExtensionResolutionV1::Quarantined {
                identity: record.identity().clone(),
                code: "integrity_unverified".into(),
                message: "exact extension integrity is not verified".into(),
            });
        }
        let Some(contribution) = record
            .manifest
            .contributions
            .iter()
            .find(|candidate| candidate.contribution_id == requirement.contribution_id)
        else {
            return Ok(ExtensionResolutionV1::Missing);
        };
        let Some(attestation) = &record.last_attestation else {
            return Ok(ExtensionResolutionV1::Unattested {
                identity: record.identity().clone(),
                expected_host_generation: host_generation,
            });
        };
        if attestation.host_id != *host_id || attestation.host_generation != host_generation {
            return Ok(ExtensionResolutionV1::Unattested {
                identity: record.identity().clone(),
                expected_host_generation: host_generation,
            });
        }
        let dependency_snapshot_hash =
            match self.dependency_snapshot_hash(record, host_id, host_generation) {
                Ok(hash) => hash,
                Err(error) if error.is_dependency_unavailable() => {
                    return Ok(ExtensionResolutionV1::Unattested {
                        identity: record.identity().clone(),
                        expected_host_generation: host_generation,
                    });
                }
                Err(error) => return Err(error),
            };
        if dependency_snapshot_hash != attestation.dependency_snapshot_hash {
            return Ok(ExtensionResolutionV1::Unattested {
                identity: record.identity().clone(),
                expected_host_generation: host_generation,
            });
        }
        Ok(ExtensionResolutionV1::Resolved {
            pin: PinnedExtensionContributionV1 {
                identity: record.identity().clone(),
                contribution: contribution.clone(),
                host_id: host_id.clone(),
                host_generation,
                handshake_hash: attestation.handshake_hash.clone(),
            },
        })
    }

    /// Builds the exact descriptor set a host generation may materialize. Any
    /// unresolved requirement blocks the entire set; partial fallback is absent.
    pub fn materialize_attested_set(
        &self,
        host_id: StableId,
        host_generation: ProcessGeneration,
        requirements: &[ExtensionRequirementV1],
    ) -> Result<AttestedExtensionSetV1, ExtensionRegistryError> {
        let mut grouped: BTreeMap<ExtensionIdentityV1, (String, Vec<_>)> = BTreeMap::new();
        for requirement in requirements {
            let resolution = self.resolve_contribution(requirement, &host_id, host_generation)?;
            let ExtensionResolutionV1::Resolved { pin } = resolution else {
                return Err(ExtensionRegistryError::RequirementUnavailable(
                    resolution_code(&resolution).into(),
                ));
            };
            let entry = grouped
                .entry(pin.identity)
                .or_insert_with(|| (pin.handshake_hash.clone(), Vec::new()));
            if entry.0 != pin.handshake_hash {
                return Err(ExtensionRegistryError::AttestationRejected(
                    "one extension identity has conflicting handshake hashes".into(),
                ));
            }
            entry.1.push(pin.contribution);
        }
        let mut extensions = grouped
            .into_iter()
            .map(|(identity, (handshake_hash, mut contributions))| {
                normalize_contributions(&mut contributions);
                AttestedExtensionV1 {
                    identity,
                    handshake_hash,
                    contributions,
                }
            })
            .collect::<Vec<_>>();
        extensions.sort_by(|left, right| left.identity.cmp(&right.identity));
        let mut set = AttestedExtensionSetV1 {
            host_id,
            host_generation,
            host_protocol: self.host_protocol,
            extensions,
            set_hash: String::new(),
        };
        set.set_hash = attested_extension_set_hash_v1(&set)?;
        Ok(set)
    }

    fn validate_handshake(
        &self,
        record: &ExtensionRecordV1,
        handshake: &HostExtensionHandshakeV1,
    ) -> Result<(), String> {
        if handshake.host_generation.0 == 0 {
            return Err("host generation must be positive".into());
        }
        if handshake.host_protocol != self.host_protocol {
            return Err("host protocol differs from the core protocol".into());
        }
        if handshake.identity != *record.identity() {
            return Err("host extension identity/version/content hash drifted".into());
        }
        if handshake.entry_point_identity != record.manifest.entry_point_identity {
            return Err("host entry-point identity drifted".into());
        }
        if !handshake.healthy {
            return Err("host reported the contribution unhealthy".into());
        }
        if handshake.contributions != record.manifest.contributions {
            return Err("host contribution descriptor set drifted".into());
        }
        let calculated = host_extension_handshake_hash_v1(handshake)
            .map_err(|_| "host handshake could not be hashed".to_owned())?;
        if calculated != handshake.handshake_hash {
            return Err("host handshake hash does not match its facts".into());
        }
        Ok(())
    }

    fn evaluate_compatibility(
        &self,
        manifest: &ExtensionManifestV1,
    ) -> ExtensionCompatibilityStatusV1 {
        let incompatible =
            |code: &str, message: &str| ExtensionCompatibilityStatusV1::Incompatible {
                code: code.into(),
                message: message.into(),
                aworkit_version: self.aworkit_version_text.clone(),
                host_protocol: self.host_protocol,
            };
        if manifest.schema_version != EXTENSION_MANIFEST_SCHEMA_V1 {
            return incompatible("manifest_schema", "manifest schema is unsupported");
        }
        let range = &manifest.compatibility;
        if range.minimum_host_protocol == 0
            || range.minimum_host_protocol > range.maximum_host_protocol
            || self.host_protocol < range.minimum_host_protocol
            || self.host_protocol > range.maximum_host_protocol
        {
            return incompatible(
                "host_protocol",
                "host protocol is outside the declared range",
            );
        }
        let Ok(minimum) = Version::parse(&range.minimum_aworkit_version) else {
            return incompatible("aworkit_range", "minimum Aworkit version is invalid");
        };
        let maximum = match &range.maximum_aworkit_version_exclusive {
            Some(value) => match Version::parse(value) {
                Ok(version) if version > minimum => Some(version),
                _ => {
                    return incompatible("aworkit_range", "maximum Aworkit version is invalid");
                }
            },
            None => None,
        };
        if self.aworkit_version < minimum
            || maximum
                .as_ref()
                .is_some_and(|maximum| self.aworkit_version >= *maximum)
        {
            return incompatible(
                "aworkit_version",
                "Aworkit version is outside the declared range",
            );
        }
        ExtensionCompatibilityStatusV1::Compatible {
            aworkit_version: self.aworkit_version_text.clone(),
            host_protocol: self.host_protocol,
        }
    }

    fn ensure_enableable(&self, record: &ExtensionRecordV1) -> Result<(), ExtensionRegistryError> {
        if !record.installed {
            return Err(ExtensionRegistryError::NotInstalled);
        }
        if record.integrity != ExtensionIntegrityStatusV1::Verified {
            return Err(ExtensionRegistryError::IntegrityUnavailable);
        }
        if !matches!(
            record.compatibility,
            ExtensionCompatibilityStatusV1::Compatible { .. }
        ) {
            return Err(ExtensionRegistryError::Incompatible);
        }
        if let Some(quarantine) = &record.quarantine {
            return Err(ExtensionRegistryError::Quarantined(quarantine.code.clone()));
        }
        for dependency in &record.manifest.dependencies {
            let minimum = Version::parse(&dependency.minimum_version)
                .map_err(|_| ExtensionRegistryError::InvalidManifest("dependency range".into()))?;
            let maximum = dependency
                .maximum_version_exclusive
                .as_deref()
                .map(Version::parse)
                .transpose()
                .map_err(|_| ExtensionRegistryError::InvalidManifest("dependency range".into()))?;
            let available =
                self.list(Some(&dependency.extension_id))?
                    .into_iter()
                    .any(|candidate| {
                        let Ok(version) = Version::parse(&candidate.identity().version) else {
                            return false;
                        };
                        candidate.installed
                            && candidate.enabled
                            && candidate.integrity == ExtensionIntegrityStatusV1::Verified
                            && matches!(
                                candidate.compatibility,
                                ExtensionCompatibilityStatusV1::Compatible { .. }
                            )
                            && candidate.quarantine.is_none()
                            && version >= minimum
                            && maximum.as_ref().is_none_or(|maximum| version < *maximum)
                    });
            if !available {
                return Err(ExtensionRegistryError::DependencyUnavailable(
                    dependency.extension_id.clone(),
                ));
            }
        }
        Ok(())
    }

    fn dependency_snapshot_hash(
        &self,
        record: &ExtensionRecordV1,
        host_id: &StableId,
        host_generation: ProcessGeneration,
    ) -> Result<String, ExtensionRegistryError> {
        let mut visiting = BTreeSet::new();
        self.dependency_snapshot_hash_inner(record, host_id, host_generation, &mut visiting)
    }

    fn dependency_snapshot_hash_inner(
        &self,
        record: &ExtensionRecordV1,
        host_id: &StableId,
        host_generation: ProcessGeneration,
        visiting: &mut BTreeSet<ExtensionIdentityV1>,
    ) -> Result<String, ExtensionRegistryError> {
        if !visiting.insert(record.identity().clone()) {
            return Err(ExtensionRegistryError::DependencyCycle(
                record.identity().extension_id.clone(),
            ));
        }
        let result = (|| {
            let mut facts = Vec::with_capacity(record.manifest.dependencies.len());
            for dependency in &record.manifest.dependencies {
                let dependency_record = self.resolve_dependency_record(dependency)?;
                let Some(attestation) = &dependency_record.last_attestation else {
                    return Err(ExtensionRegistryError::DependencyAttestationStale(
                        dependency.extension_id.clone(),
                    ));
                };
                if attestation.host_id != *host_id || attestation.host_generation != host_generation
                {
                    return Err(ExtensionRegistryError::DependencyAttestationStale(
                        dependency.extension_id.clone(),
                    ));
                }
                let nested_hash = self.dependency_snapshot_hash_inner(
                    &dependency_record,
                    host_id,
                    host_generation,
                    visiting,
                )?;
                if nested_hash != attestation.dependency_snapshot_hash {
                    return Err(ExtensionRegistryError::DependencyAttestationStale(
                        dependency.extension_id.clone(),
                    ));
                }
                facts.push(DependencyAttestationFactV1 {
                    identity: dependency_record.identity().clone(),
                    record_version: dependency_record.record_version,
                    handshake_hash: attestation.handshake_hash.clone(),
                    descriptor_set_hash: attestation.descriptor_set_hash.clone(),
                    dependency_snapshot_hash: nested_hash,
                });
            }
            let bytes = serde_jcs::to_vec(&facts)
                .map_err(|_| ExtensionRegistryError::DependencySnapshotEncoding)?;
            Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
        })();
        visiting.remove(record.identity());
        result
    }

    fn resolve_dependency_record(
        &self,
        dependency: &ExtensionDependencyV1,
    ) -> Result<ExtensionRecordV1, ExtensionRegistryError> {
        let minimum = Version::parse(&dependency.minimum_version)
            .map_err(|_| ExtensionRegistryError::InvalidManifest("dependency range".into()))?;
        let maximum = dependency
            .maximum_version_exclusive
            .as_deref()
            .map(Version::parse)
            .transpose()
            .map_err(|_| ExtensionRegistryError::InvalidManifest("dependency range".into()))?;
        let mut candidates = self
            .list(Some(&dependency.extension_id))?
            .into_iter()
            .filter(|candidate| {
                let Ok(version) = Version::parse(&candidate.identity().version) else {
                    return false;
                };
                candidate.installed
                    && candidate.enabled
                    && candidate.integrity == ExtensionIntegrityStatusV1::Verified
                    && matches!(
                        candidate.compatibility,
                        ExtensionCompatibilityStatusV1::Compatible { .. }
                    )
                    && candidate.quarantine.is_none()
                    && version >= minimum
                    && maximum.as_ref().is_none_or(|maximum| version < *maximum)
            })
            .collect::<Vec<_>>();
        match candidates.len() {
            0 => Err(ExtensionRegistryError::DependencyUnavailable(
                dependency.extension_id.clone(),
            )),
            1 => Ok(candidates.pop().expect("one dependency candidate")),
            _ => Err(ExtensionRegistryError::DependencyAmbiguous(
                dependency.extension_id.clone(),
            )),
        }
    }

    fn write_quarantine(
        &self,
        operation_id: StableId,
        mut record: ExtensionRecordV1,
        expected_version: u64,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<ExtensionRecordV1, ExtensionRegistryError> {
        record.quarantine = Some(ExtensionQuarantineV1 {
            code: code.into(),
            message: message.into(),
        });
        record.last_attestation = None;
        record.record_version = next_version(expected_version)?;
        self.write(ExtensionInventoryWriteV1 {
            operation_id,
            expected_version: Some(expected_version),
            record,
            audit_kind: ExtensionAuditKindV1::Quarantined,
            detail: "quarantined exact extension identity for new dispatch".into(),
        })
    }

    fn require_record(
        &self,
        identity: &ExtensionIdentityV1,
    ) -> Result<ExtensionRecordV1, ExtensionRegistryError> {
        self.load(identity)?.ok_or(ExtensionRegistryError::Missing)
    }

    fn load(
        &self,
        identity: &ExtensionIdentityV1,
    ) -> Result<Option<ExtensionRecordV1>, ExtensionRegistryError> {
        self.inventory
            .load(identity)
            .map_err(ExtensionRegistryError::inventory)
    }

    fn list(
        &self,
        extension_id: Option<&StableId>,
    ) -> Result<Vec<ExtensionRecordV1>, ExtensionRegistryError> {
        self.inventory
            .list(extension_id)
            .map_err(ExtensionRegistryError::inventory)
    }

    fn write(
        &self,
        request: ExtensionInventoryWriteV1,
    ) -> Result<ExtensionRecordV1, ExtensionRegistryError> {
        self.inventory
            .write(&request)
            .map_err(ExtensionRegistryError::inventory)
    }
}

fn validate_identity_key(manifest: &ExtensionManifestV1) -> Result<(), ExtensionRegistryError> {
    if manifest.identity.version.is_empty()
        || manifest.identity.version.len() > 128
        || !is_canonical_sha256(&manifest.identity.content_hash)
    {
        return Err(ExtensionRegistryError::InvalidManifest(
            "exact identity key".into(),
        ));
    }
    Ok(())
}

fn evaluate_integrity(candidate: &InertExtensionCandidateV1) -> ExtensionIntegrityStatusV1 {
    let manifest = &candidate.manifest;
    let unique_capabilities = manifest
        .contributions
        .iter()
        .map(|contribution| contribution.descriptor.capability_id.as_str())
        .collect::<BTreeSet<_>>()
        .len()
        == manifest.contributions.len();
    if manifest.contributions.len() > MAX_MANIFEST_CONTRIBUTIONS
        || manifest.dependencies.len() > MAX_MANIFEST_DEPENDENCIES
        || !unique_capabilities
        || !valid_manifest_text(&manifest.entry_point_identity, 4096)
        || manifest
            .configuration_schema_hash
            .as_ref()
            .is_some_and(|hash| !is_canonical_sha256(hash))
        || manifest
            .contributions
            .windows(2)
            .any(|pair| pair[0].contribution_id >= pair[1].contribution_id)
        || manifest.contributions.iter().any(|contribution| {
            validate_descriptor(&contribution.descriptor).is_err()
                || !descriptor_hash_matches(&contribution.descriptor)
        })
    {
        return ExtensionIntegrityStatusV1::MalformedManifest(
            "manifest or contribution descriptor is malformed".into(),
        );
    }
    if !is_canonical_sha256(&candidate.observed_content_hash)
        || candidate.observed_content_hash != manifest.identity.content_hash
    {
        return ExtensionIntegrityStatusV1::ContentHashMismatch;
    }
    let Some(observed_entry_point) = &candidate.observed_entry_point_identity else {
        return ExtensionIntegrityStatusV1::MissingEntryPoint;
    };
    if observed_entry_point != &manifest.entry_point_identity {
        return ExtensionIntegrityStatusV1::EntryPointIdentityMismatch;
    }
    ExtensionIntegrityStatusV1::Verified
}

fn validate_descriptor(descriptor: &CapabilityDescriptorV1) -> Result<(), ()> {
    if descriptor.adapter_version.is_empty()
        || descriptor.adapter_version.len() > 128
        || descriptor.maximum_concurrency == 0
        || descriptor.max_input_bytes == 0
        || descriptor.max_output_bytes == 0
        || !is_canonical_sha256(&descriptor.descriptor_hash)
        || !sorted_unique_text(&descriptor.allowed_scopes)
        || !sorted_unique_text(&descriptor.secret_slots)
        || !sorted_unique_text(&descriptor.supported_platforms)
        || descriptor
            .input_schema_hash
            .as_ref()
            .is_some_and(|hash| !is_canonical_sha256(hash))
        || descriptor
            .output_schema_hash
            .as_ref()
            .is_some_and(|hash| !is_canonical_sha256(hash))
    {
        Err(())
    } else {
        Ok(())
    }
}

fn descriptor_hash_matches(descriptor: &CapabilityDescriptorV1) -> bool {
    match capability_descriptor_hash_v1(descriptor) {
        Ok(hash) => hash == descriptor.descriptor_hash,
        Err(_) => false,
    }
}

fn sorted_unique_text(values: &[String]) -> bool {
    values.iter().all(|value| valid_manifest_text(value, 256))
        && values.windows(2).all(|pair| pair[0] < pair[1])
}

fn valid_manifest_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.contains('\0')
}

fn registration_quarantine(
    integrity: &ExtensionIntegrityStatusV1,
    compatibility: &ExtensionCompatibilityStatusV1,
) -> Option<ExtensionQuarantineV1> {
    if integrity != &ExtensionIntegrityStatusV1::Verified {
        return Some(ExtensionQuarantineV1 {
            code: "integrity_failed".into(),
            message: integrity_message(integrity).into(),
        });
    }
    if let ExtensionCompatibilityStatusV1::Incompatible { code, message, .. } = compatibility {
        return Some(ExtensionQuarantineV1 {
            code: format!("compatibility_{code}"),
            message: message.clone(),
        });
    }
    None
}

fn integrity_message(status: &ExtensionIntegrityStatusV1) -> &'static str {
    match status {
        ExtensionIntegrityStatusV1::Unverified => "extension integrity is unverified",
        ExtensionIntegrityStatusV1::Verified => "extension integrity is verified",
        ExtensionIntegrityStatusV1::MissingEntryPoint => "extension entry point is missing",
        ExtensionIntegrityStatusV1::EntryPointIdentityMismatch => {
            "extension entry-point identity changed"
        }
        ExtensionIntegrityStatusV1::ContentHashMismatch => "extension content hash changed",
        ExtensionIntegrityStatusV1::MalformedManifest(_) => "extension manifest is malformed",
    }
}

fn normalize_manifest(manifest: &mut ExtensionManifestV1) {
    normalize_contributions(&mut manifest.contributions);
    manifest.dependencies.sort_by(|left, right| {
        left.extension_id
            .cmp(&right.extension_id)
            .then_with(|| left.minimum_version.cmp(&right.minimum_version))
            .then_with(|| {
                left.maximum_version_exclusive
                    .cmp(&right.maximum_version_exclusive)
            })
    });
}

fn normalize_contributions(contributions: &mut [aworkit_protocol::ExtensionContributionV1]) {
    contributions.sort_by(|left, right| {
        left.contribution_id
            .cmp(&right.contribution_id)
            .then_with(|| {
                left.descriptor
                    .capability_id
                    .cmp(&right.descriptor.capability_id)
            })
    });
    for contribution in contributions {
        for values in [
            &mut contribution.descriptor.allowed_scopes,
            &mut contribution.descriptor.secret_slots,
            &mut contribution.descriptor.supported_platforms,
        ] {
            values.sort();
            values.dedup();
        }
    }
}

fn ensure_expected_version(
    record: &ExtensionRecordV1,
    expected: u64,
) -> Result<(), ExtensionRegistryError> {
    if record.record_version == expected {
        Ok(())
    } else {
        Err(ExtensionRegistryError::VersionConflict {
            expected,
            actual: record.record_version,
        })
    }
}

fn next_version(current: u64) -> Result<u64, ExtensionRegistryError> {
    current
        .checked_add(1)
        .ok_or(ExtensionRegistryError::VersionExhausted)
}

fn one_extension_set_hash(
    handshake: &HostExtensionHandshakeV1,
) -> Result<String, ExtensionRegistryError> {
    let set = AttestedExtensionSetV1 {
        host_id: handshake.host_id.clone(),
        host_generation: handshake.host_generation,
        host_protocol: handshake.host_protocol,
        extensions: vec![AttestedExtensionV1 {
            identity: handshake.identity.clone(),
            handshake_hash: handshake.handshake_hash.clone(),
            contributions: handshake.contributions.clone(),
        }],
        set_hash: String::new(),
    };
    Ok(attested_extension_set_hash_v1(&set)?)
}

fn resolution_code(resolution: &ExtensionResolutionV1) -> &'static str {
    match resolution {
        ExtensionResolutionV1::Resolved { .. } => "resolved",
        ExtensionResolutionV1::Missing => "missing",
        ExtensionResolutionV1::Disabled { .. } => "disabled",
        ExtensionResolutionV1::Incompatible { .. } => "incompatible",
        ExtensionResolutionV1::Drifted { .. } => "drifted",
        ExtensionResolutionV1::Quarantined { .. } => "quarantined",
        ExtensionResolutionV1::Unattested { .. } => "unattested",
    }
}

/// Trusted-extension validation, persistence, and resolution failures.
#[derive(Debug, Error)]
pub enum ExtensionRegistryError {
    #[error("Aworkit version is not valid semantic versioning")]
    InvalidAworkitVersion,
    #[error("host protocol must be positive")]
    InvalidHostProtocol,
    #[error("extension manifest is invalid: {0}")]
    InvalidManifest(String),
    #[error("an exact extension identity cannot change its manifest")]
    ExactIdentityMutation,
    #[error("extension inventory record does not exist")]
    Missing,
    #[error("extension is not installed")]
    NotInstalled,
    #[error("extension integrity is unavailable")]
    IntegrityUnavailable,
    #[error("extension is incompatible")]
    Incompatible,
    #[error("extension is quarantined: {0}")]
    Quarantined(String),
    #[error("extension dependency {0} is unavailable")]
    DependencyUnavailable(StableId),
    #[error("extension dependency {0} has multiple enabled compatible identities")]
    DependencyAmbiguous(StableId),
    #[error("extension dependency {0} attestation is stale")]
    DependencyAttestationStale(StableId),
    #[error("extension dependency cycle includes {0}")]
    DependencyCycle(StableId),
    #[error("extension dependency snapshot could not be encoded")]
    DependencySnapshotEncoding,
    #[error("extension inventory version conflict: expected {expected}, found {actual}")]
    VersionConflict { expected: u64, actual: u64 },
    #[error("extension inventory record version is exhausted")]
    VersionExhausted,
    #[error("stale host generation: previous {previous}, received {received}")]
    StaleHostGeneration { previous: u64, received: u64 },
    #[error("host extension attestation was rejected: {0}")]
    AttestationRejected(String),
    #[error("required extension contribution is unavailable: {0}")]
    RequirementUnavailable(String),
    #[error("extension inventory failed ({code}): {message}")]
    Inventory { code: String, message: String },
    #[error(transparent)]
    Protocol(#[from] ExtensionProtocolError),
}

impl ExtensionRegistryError {
    fn inventory(error: ExtensionInventoryPortErrorV1) -> Self {
        if error.code == "version_conflict" {
            return Self::Inventory {
                code: error.code,
                message: error.message,
            };
        }
        Self::Inventory {
            code: error.code,
            message: error.message,
        }
    }

    fn is_dependency_unavailable(&self) -> bool {
        matches!(
            self,
            Self::DependencyUnavailable(_)
                | Self::DependencyAmbiguous(_)
                | Self::DependencyAttestationStale(_)
                | Self::DependencyCycle(_)
        )
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DependencyAttestationFactV1 {
    identity: ExtensionIdentityV1,
    record_version: u64,
    handshake_hash: String,
    descriptor_set_hash: String,
    dependency_snapshot_hash: String,
}
