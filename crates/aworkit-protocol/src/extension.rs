//! Process-neutral trusted-extension inventory and host-attestation contracts.
//!
//! These values contain metadata only. Parsing them never installs, enables,
//! loads, or executes extension code.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{ProcessGeneration, StableId};

/// Manifest schema understood by the first trusted-extension registry.
pub const EXTENSION_MANIFEST_SCHEMA_V1: u16 = 1;

/// Core-to-host attestation protocol understood by this release.
pub const EXTENSION_HOST_PROTOCOL_V1: u16 = 1;

/// Exact installed-code identity. A changed version or content hash is new code.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionIdentityV1 {
    pub extension_id: StableId,
    pub version: String,
    pub content_hash: String,
}

/// Capability classes that can be contributed to the extension host.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKindV1 {
    Shell,
    Python,
    Model,
    FileRead,
    FileSearch,
    FileEdit,
    Mcp,
    Plugin,
    ExternalAgent,
    Isolation,
}

/// Conservative side-effect semantics declared by an adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySideEffectV1 {
    Pure,
    ReadOnly,
    IdempotentWrite,
    NonIdempotent,
    Unknown,
}

/// Truthful visibility and trust boundary of a contributed adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityVisibilityV1 {
    Mediated,
    Opaque,
    TrustedDesktopUser,
}

/// Complete provider-neutral descriptor hashed and pinned by the core.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityDescriptorV1 {
    pub capability_id: StableId,
    pub adapter_version: String,
    pub kind: CapabilityKindV1,
    pub side_effect: CapabilitySideEffectV1,
    pub guarantees_same_id_deduplication: bool,
    pub supports_streaming: bool,
    pub supports_cancellation: bool,
    pub supports_continuation: bool,
    pub supports_sessions: bool,
    pub supports_approval_forwarding: bool,
    pub supports_mcp_forwarding: bool,
    pub allowed_scopes: Vec<String>,
    pub secret_slots: Vec<String>,
    pub input_schema_hash: Option<String>,
    pub output_schema_hash: Option<String>,
    pub requires_workspace: bool,
    pub required_isolation: Option<String>,
    pub maximum_concurrency: u32,
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
    pub supported_platforms: Vec<String>,
    pub visibility: CapabilityVisibilityV1,
    /// SHA-256 of this descriptor with this field empty and set-like fields sorted.
    pub descriptor_hash: String,
}

/// Stable contribution identity plus its exact executable descriptor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionContributionV1 {
    pub contribution_id: StableId,
    pub descriptor: CapabilityDescriptorV1,
}

/// One exact-version dependency declared without resolving or installing it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionDependencyV1 {
    pub extension_id: StableId,
    pub minimum_version: String,
    pub maximum_version_exclusive: Option<String>,
}

/// Non-secret provenance and signature evidence retained for inspection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionProvenanceV1 {
    pub source: String,
    pub publisher: Option<String>,
    pub signature_status: String,
    pub signature_identity: Option<String>,
}

/// Compatibility range declared by an inert manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionCompatibilityRangeV1 {
    pub minimum_aworkit_version: String,
    pub maximum_aworkit_version_exclusive: Option<String>,
    pub minimum_host_protocol: u16,
    pub maximum_host_protocol: u16,
}

/// Parsed metadata for one exact extension package. Entry-point identity is
/// descriptive; this DTO provides no filesystem or process operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionManifestV1 {
    pub schema_version: u16,
    pub identity: ExtensionIdentityV1,
    pub compatibility: ExtensionCompatibilityRangeV1,
    pub entry_point_identity: String,
    pub configuration_schema_hash: Option<String>,
    pub contributions: Vec<ExtensionContributionV1>,
    pub dependencies: Vec<ExtensionDependencyV1>,
    pub provenance: ExtensionProvenanceV1,
}

/// Bounded discovery facts produced without running the candidate extension.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InertExtensionCandidateV1 {
    pub manifest: ExtensionManifestV1,
    pub observed_content_hash: String,
    pub observed_entry_point_identity: Option<String>,
}

/// Durable integrity state determined by the trusted core.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum ExtensionIntegrityStatusV1 {
    Unverified,
    Verified,
    MissingEntryPoint,
    EntryPointIdentityMismatch,
    ContentHashMismatch,
    MalformedManifest(String),
}

/// Durable compatibility result evaluated against an exact application build.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionCompatibilityStatusV1 {
    Unchecked,
    Compatible {
        aworkit_version: String,
        host_protocol: u16,
    },
    Incompatible {
        code: String,
        message: String,
        aworkit_version: String,
        host_protocol: u16,
    },
}

/// Explicit quarantine facts. Quarantine never substitutes another version.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionQuarantineV1 {
    pub code: String,
    pub message: String,
}

/// Metadata-only descriptor handshake from one supervised host generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostExtensionHandshakeV1 {
    pub host_id: StableId,
    pub host_generation: ProcessGeneration,
    pub host_protocol: u16,
    pub identity: ExtensionIdentityV1,
    pub entry_point_identity: String,
    pub healthy: bool,
    pub contributions: Vec<ExtensionContributionV1>,
    /// SHA-256 of this handshake with this field empty and contributions sorted.
    pub handshake_hash: String,
}

/// Core-accepted attestation retained against an exact inventory record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostExtensionAttestationV1 {
    pub host_id: StableId,
    pub host_generation: ProcessGeneration,
    pub host_protocol: u16,
    pub handshake_hash: String,
    pub descriptor_set_hash: String,
    /// Exact transitive dependency records and attestations accepted with this handshake.
    #[serde(default)]
    pub dependency_snapshot_hash: String,
}

/// Complete durable state for one exact installed extension identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionRecordV1 {
    pub manifest: ExtensionManifestV1,
    pub installed: bool,
    pub enabled: bool,
    pub integrity: ExtensionIntegrityStatusV1,
    pub compatibility: ExtensionCompatibilityStatusV1,
    pub quarantine: Option<ExtensionQuarantineV1>,
    pub record_version: u64,
    pub last_attestation: Option<HostExtensionAttestationV1>,
}

impl ExtensionRecordV1 {
    /// Returns the exact package identity used by persistence and resolution.
    #[must_use]
    pub fn identity(&self) -> &ExtensionIdentityV1 {
        &self.manifest.identity
    }
}

/// Immutable audit fact written atomically with one inventory mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionAuditKindV1 {
    Registered,
    EnablementChanged,
    IntegrityEvaluated,
    CompatibilityEvaluated,
    Quarantined,
    Attested,
    Removed,
}

/// Compare-and-swap mutation accepted by the inventory persistence port.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionInventoryWriteV1 {
    pub operation_id: StableId,
    pub expected_version: Option<u64>,
    pub record: ExtensionRecordV1,
    pub audit_kind: ExtensionAuditKindV1,
    pub detail: String,
}

/// Immutable audit row returned without storage-native values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionAuditEntryV1 {
    pub sequence: u64,
    pub operation_id: StableId,
    pub identity: ExtensionIdentityV1,
    pub record_version: u64,
    pub kind: ExtensionAuditKindV1,
    pub prior_record_hash: Option<String>,
    pub record_hash: String,
    pub detail: String,
    pub record: ExtensionRecordV1,
}

/// Stable persistence error containing no SQLite-native value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionInventoryPortErrorV1 {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub inspectable_read_only: bool,
}

/// Core-owned extension inventory port. Implementations persist facts only and
/// must not discover, hash, load, or execute extension code.
pub trait ExtensionInventoryPort: Send + Sync {
    fn load(
        &self,
        identity: &ExtensionIdentityV1,
    ) -> Result<Option<ExtensionRecordV1>, ExtensionInventoryPortErrorV1>;
    fn list(
        &self,
        extension_id: Option<&StableId>,
    ) -> Result<Vec<ExtensionRecordV1>, ExtensionInventoryPortErrorV1>;
    fn find_by_contribution(
        &self,
        contribution_id: &StableId,
    ) -> Result<Vec<ExtensionRecordV1>, ExtensionInventoryPortErrorV1>;
    fn write(
        &self,
        request: &ExtensionInventoryWriteV1,
    ) -> Result<ExtensionRecordV1, ExtensionInventoryPortErrorV1>;
    fn audit_entries(
        &self,
        after_sequence: u64,
        limit: u32,
    ) -> Result<Vec<ExtensionAuditEntryV1>, ExtensionInventoryPortErrorV1>;
}

/// Exact workflow/configuration requirement resolved without fallback.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionRequirementV1 {
    pub extension_id: StableId,
    pub contribution_id: StableId,
    pub exact_version: String,
    pub exact_content_hash: String,
}

/// Exact contribution pinned to one successfully attested host generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PinnedExtensionContributionV1 {
    pub identity: ExtensionIdentityV1,
    pub contribution: ExtensionContributionV1,
    pub host_id: StableId,
    pub host_generation: ProcessGeneration,
    pub handshake_hash: String,
}

/// Minimal executable provenance retained anywhere a contributed capability is
/// frozen or dispatched. The descriptor hash remains a separate capability
/// binding field; this value prevents identical descriptors from silently
/// switching to different installed code or a different host handshake.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionRuntimeBindingV1 {
    pub identity: ExtensionIdentityV1,
    pub contribution_id: StableId,
    pub host_id: StableId,
    pub host_generation: ProcessGeneration,
    pub handshake_hash: String,
}

impl PinnedExtensionContributionV1 {
    /// Projects the full core resolution into the provenance required at the
    /// worker and capability-host boundaries without carrying executable code.
    #[must_use]
    pub fn runtime_binding(&self) -> ExtensionRuntimeBindingV1 {
        ExtensionRuntimeBindingV1 {
            identity: self.identity.clone(),
            contribution_id: self.contribution.contribution_id.clone(),
            host_id: self.host_id.clone(),
            host_generation: self.host_generation,
            handshake_hash: self.handshake_hash.clone(),
        }
    }
}

/// Fail-closed requirement result that preserves why execution is unavailable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtensionResolutionV1 {
    Resolved {
        pin: PinnedExtensionContributionV1,
    },
    Missing,
    Disabled {
        identity: ExtensionIdentityV1,
    },
    Incompatible {
        identity: ExtensionIdentityV1,
        code: String,
        message: String,
    },
    Drifted {
        expected_version: String,
        expected_content_hash: String,
        installed_identities: Vec<ExtensionIdentityV1>,
    },
    Quarantined {
        identity: ExtensionIdentityV1,
        code: String,
        message: String,
    },
    Unattested {
        identity: ExtensionIdentityV1,
        expected_host_generation: ProcessGeneration,
    },
}

/// One exact extension included in a core-approved host descriptor set.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttestedExtensionV1 {
    pub identity: ExtensionIdentityV1,
    pub handshake_hash: String,
    pub contributions: Vec<ExtensionContributionV1>,
}

/// Immutable adapter set that a host may materialize for one generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttestedExtensionSetV1 {
    pub host_id: StableId,
    pub host_generation: ProcessGeneration,
    pub host_protocol: u16,
    pub extensions: Vec<AttestedExtensionV1>,
    /// SHA-256 of this set with this field empty and all identities sorted.
    pub set_hash: String,
}

/// Computes the exact adapter descriptor identity shared by core and host.
pub fn capability_descriptor_hash_v1(
    descriptor: &CapabilityDescriptorV1,
) -> Result<String, ExtensionProtocolError> {
    let mut canonical = descriptor.clone();
    canonical.descriptor_hash.clear();
    normalize_descriptor(&mut canonical);
    canonical_hash(&canonical)
}

/// Computes the exact metadata-only host handshake identity.
pub fn host_extension_handshake_hash_v1(
    handshake: &HostExtensionHandshakeV1,
) -> Result<String, ExtensionProtocolError> {
    let mut canonical = handshake.clone();
    canonical.handshake_hash.clear();
    canonical.contributions.sort_by(|left, right| {
        left.contribution_id
            .as_str()
            .cmp(right.contribution_id.as_str())
    });
    for contribution in &mut canonical.contributions {
        normalize_descriptor(&mut contribution.descriptor);
    }
    canonical_hash(&canonical)
}

/// Computes the identity of a complete core-approved host adapter set.
pub fn attested_extension_set_hash_v1(
    set: &AttestedExtensionSetV1,
) -> Result<String, ExtensionProtocolError> {
    let mut canonical = set.clone();
    canonical.set_hash.clear();
    canonical.extensions.sort_by(|left, right| {
        left.identity
            .cmp(&right.identity)
            .then_with(|| left.handshake_hash.cmp(&right.handshake_hash))
    });
    for extension in &mut canonical.extensions {
        extension.contributions.sort_by(|left, right| {
            left.contribution_id
                .as_str()
                .cmp(right.contribution_id.as_str())
        });
        for contribution in &mut extension.contributions {
            normalize_descriptor(&mut contribution.descriptor);
        }
    }
    canonical_hash(&canonical)
}

/// Returns whether a value is a canonical lowercase `sha256:` identity.
#[must_use]
pub fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn normalize_descriptor(descriptor: &mut CapabilityDescriptorV1) {
    for values in [
        &mut descriptor.allowed_scopes,
        &mut descriptor.secret_slots,
        &mut descriptor.supported_platforms,
    ] {
        values.sort();
        values.dedup();
    }
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<String, ExtensionProtocolError> {
    let bytes = serde_jcs::to_vec(value).map_err(|_| ExtensionProtocolError::Encoding)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

/// Deterministic hashing failures for extension boundary DTOs.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ExtensionProtocolError {
    #[error("extension protocol value could not be encoded canonically")]
    Encoding,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> StableId {
        StableId::parse(value).expect("stable ID")
    }

    fn descriptor() -> CapabilityDescriptorV1 {
        let mut descriptor = CapabilityDescriptorV1 {
            capability_id: id("tool.search"),
            adapter_version: "1.2.3".into(),
            kind: CapabilityKindV1::FileSearch,
            side_effect: CapabilitySideEffectV1::ReadOnly,
            guarantees_same_id_deduplication: true,
            supports_streaming: true,
            supports_cancellation: true,
            supports_continuation: false,
            supports_sessions: false,
            supports_approval_forwarding: false,
            supports_mcp_forwarding: false,
            allowed_scopes: vec!["project.search".into(), "project.read".into()],
            secret_slots: Vec::new(),
            input_schema_hash: None,
            output_schema_hash: None,
            requires_workspace: true,
            required_isolation: None,
            maximum_concurrency: 4,
            max_input_bytes: 1024,
            max_output_bytes: 4096,
            supported_platforms: vec!["linux".into(), "windows".into()],
            visibility: CapabilityVisibilityV1::Mediated,
            descriptor_hash: String::new(),
        };
        descriptor.descriptor_hash =
            capability_descriptor_hash_v1(&descriptor).expect("descriptor hash");
        descriptor
    }

    #[test]
    fn descriptor_hash_normalizes_set_like_fields() {
        let descriptor = descriptor();
        let mut reordered = descriptor.clone();
        reordered.allowed_scopes.reverse();
        reordered.supported_platforms.reverse();
        assert_eq!(
            capability_descriptor_hash_v1(&descriptor).expect("first"),
            capability_descriptor_hash_v1(&reordered).expect("second")
        );
        assert!(is_canonical_sha256(&descriptor.descriptor_hash));
    }

    #[test]
    fn handshake_hash_is_generation_and_descriptor_fenced() {
        let contribution = ExtensionContributionV1 {
            contribution_id: id("contribution.search"),
            descriptor: descriptor(),
        };
        let mut handshake = HostExtensionHandshakeV1 {
            host_id: id("host.primary"),
            host_generation: ProcessGeneration(7),
            host_protocol: EXTENSION_HOST_PROTOCOL_V1,
            identity: ExtensionIdentityV1 {
                extension_id: id("extension.search"),
                version: "1.2.3".into(),
                content_hash: format!("sha256:{}", "a".repeat(64)),
            },
            entry_point_identity: "bin/search-adapter".into(),
            healthy: true,
            contributions: vec![contribution],
            handshake_hash: String::new(),
        };
        handshake.handshake_hash =
            host_extension_handshake_hash_v1(&handshake).expect("handshake hash");
        let original = handshake.handshake_hash.clone();
        handshake.host_generation = ProcessGeneration(8);
        assert_ne!(
            original,
            host_extension_handshake_hash_v1(&handshake).expect("changed hash")
        );
    }
}
