//! Inert extension-manifest parsing and exact trusted-core pin verification.

use std::collections::{BTreeMap, BTreeSet};

use aworkit_protocol::{ProcessGeneration, SchemaVersion, StableId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const SHA256_PREFIX: &str = "sha256:";

/// Bounds applied before and after parsing untrusted manifest bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginManifestLimitsV1 {
    pub maximum_manifest_bytes: usize,
    pub maximum_text_bytes: usize,
    pub maximum_entry_arguments: usize,
    pub maximum_contributions: usize,
    pub maximum_dependencies: usize,
    pub maximum_schema_bytes: usize,
}

impl Default for PluginManifestLimitsV1 {
    fn default() -> Self {
        Self {
            maximum_manifest_bytes: 256 * 1024,
            maximum_text_bytes: 8 * 1024,
            maximum_entry_arguments: 256,
            maximum_contributions: 256,
            maximum_dependencies: 256,
            maximum_schema_bytes: 64 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginContributionKindV1 {
    Node,
    Tool,
    Provider,
    Evaluator,
    Adapter,
}

/// An argv-only entry point. Parsing never resolves, opens, or executes it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginEntryPointV1 {
    pub program: String,
    #[serde(default)]
    pub arguments: Vec<String>,
}

/// One language-neutral contribution descriptor.
///
/// Unknown contribution fields are retained as bounded opaque evidence so a
/// newer descriptor is not silently rewritten by an older host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginContributionV1 {
    pub contribution_id: StableId,
    pub kind: PluginContributionKindV1,
    pub input_schema: Value,
    pub output_schema: Value,
    #[serde(default, flatten)]
    pub opaque_fields: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginDependencyV1 {
    pub extension_id: StableId,
    pub version_requirement: String,
}

/// Manifest data parsed without consulting the filesystem or starting code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionManifestV1 {
    pub schema_version: SchemaVersion,
    pub extension_id: StableId,
    pub version: String,
    pub content_hash: String,
    pub aworkit_version_requirement: String,
    pub protocol_version: u16,
    pub entry_point: PluginEntryPointV1,
    pub contributions: Vec<PluginContributionV1>,
    #[serde(default)]
    pub dependencies: Vec<PluginDependencyV1>,
}

/// Core-owned facts that authorize exactly one manifest in one host generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttestedPluginPinV1 {
    pub extension_id: StableId,
    pub version: String,
    pub content_hash: String,
    pub protocol_version: u16,
    pub contribution_ids: Vec<StableId>,
    pub host_generation: ProcessGeneration,
    pub enabled: bool,
    pub compatible: bool,
}

/// A manifest whose identity and complete contribution set match a core pin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedPluginManifestV1 {
    manifest: ExtensionManifestV1,
    pin: AttestedPluginPinV1,
}

impl PinnedPluginManifestV1 {
    /// Verifies enablement, compatibility, identity, version, hash, protocol,
    /// contribution set, and the active host generation without launching code.
    pub fn verify(
        manifest: ExtensionManifestV1,
        pin: AttestedPluginPinV1,
        active_host_generation: ProcessGeneration,
    ) -> Result<Self, PluginPinError> {
        if !pin.enabled {
            return Err(PluginPinError::Disabled);
        }
        if !pin.compatible {
            return Err(PluginPinError::Incompatible);
        }
        if pin.host_generation != active_host_generation {
            return Err(PluginPinError::HostGenerationDrift);
        }
        if manifest.extension_id != pin.extension_id {
            return Err(PluginPinError::IdentityDrift);
        }
        if manifest.version != pin.version {
            return Err(PluginPinError::VersionDrift);
        }
        if manifest.content_hash != pin.content_hash {
            return Err(PluginPinError::ContentHashDrift);
        }
        if manifest.protocol_version != pin.protocol_version {
            return Err(PluginPinError::ProtocolDrift);
        }
        let manifest_ids = contribution_id_set(
            manifest
                .contributions
                .iter()
                .map(|value| &value.contribution_id),
        );
        let pinned_ids = contribution_id_set(pin.contribution_ids.iter());
        if manifest_ids.len() != manifest.contributions.len()
            || pinned_ids.len() != pin.contribution_ids.len()
            || manifest_ids != pinned_ids
        {
            return Err(PluginPinError::ContributionDrift);
        }
        Ok(Self { manifest, pin })
    }

    #[must_use]
    pub fn manifest(&self) -> &ExtensionManifestV1 {
        &self.manifest
    }

    #[must_use]
    pub fn pin(&self) -> &AttestedPluginPinV1 {
        &self.pin
    }

    #[must_use]
    pub fn host_generation(&self) -> ProcessGeneration {
        self.pin.host_generation
    }

    #[must_use]
    pub fn permits_contribution(&self, contribution_id: &StableId) -> bool {
        self.pin
            .contribution_ids
            .iter()
            .any(|value| value == contribution_id)
    }
}

/// Parses and validates manifest bytes without performing filesystem or process I/O.
pub fn parse_extension_manifest_v1(
    bytes: &[u8],
    limits: PluginManifestLimitsV1,
) -> Result<ExtensionManifestV1, PluginManifestError> {
    validate_limits(limits)?;
    if bytes.is_empty() || bytes.len() > limits.maximum_manifest_bytes {
        return Err(PluginManifestError::ManifestSize);
    }
    let manifest: ExtensionManifestV1 = serde_json::from_slice(bytes)?;
    validate_manifest(&manifest, limits)?;
    Ok(manifest)
}

fn validate_limits(limits: PluginManifestLimitsV1) -> Result<(), PluginManifestError> {
    if limits.maximum_manifest_bytes == 0
        || limits.maximum_text_bytes == 0
        || limits.maximum_entry_arguments == 0
        || limits.maximum_contributions == 0
        || limits.maximum_schema_bytes == 0
    {
        return Err(PluginManifestError::InvalidLimits);
    }
    Ok(())
}

fn validate_manifest(
    manifest: &ExtensionManifestV1,
    limits: PluginManifestLimitsV1,
) -> Result<(), PluginManifestError> {
    if manifest.schema_version != SchemaVersion::V1 {
        return Err(PluginManifestError::UnsupportedSchemaVersion(
            manifest.schema_version.0,
        ));
    }
    validate_text(&manifest.version, limits.maximum_text_bytes)?;
    validate_sha256(&manifest.content_hash)?;
    validate_text(
        &manifest.aworkit_version_requirement,
        limits.maximum_text_bytes,
    )?;
    if manifest.protocol_version == 0 {
        return Err(PluginManifestError::InvalidProtocolVersion);
    }
    validate_text(&manifest.entry_point.program, limits.maximum_text_bytes)?;
    if manifest.entry_point.program.contains('\0')
        || manifest.entry_point.arguments.len() > limits.maximum_entry_arguments
    {
        return Err(PluginManifestError::InvalidEntryPoint);
    }
    for argument in &manifest.entry_point.arguments {
        validate_text_allow_empty(argument, limits.maximum_text_bytes)?;
        if argument.contains('\0') {
            return Err(PluginManifestError::InvalidEntryPoint);
        }
    }
    if manifest.contributions.is_empty()
        || manifest.contributions.len() > limits.maximum_contributions
    {
        return Err(PluginManifestError::ContributionCount);
    }
    let mut contribution_ids = BTreeSet::new();
    for contribution in &manifest.contributions {
        if !contribution_ids.insert(contribution.contribution_id.as_str()) {
            return Err(PluginManifestError::DuplicateContribution);
        }
        validate_json(&contribution.input_schema, limits.maximum_schema_bytes)?;
        validate_json(&contribution.output_schema, limits.maximum_schema_bytes)?;
        validate_json(
            &Value::Object(contribution.opaque_fields.clone().into_iter().collect()),
            limits.maximum_schema_bytes,
        )?;
    }
    if manifest.dependencies.len() > limits.maximum_dependencies {
        return Err(PluginManifestError::DependencyCount);
    }
    let mut dependency_ids = BTreeSet::new();
    for dependency in &manifest.dependencies {
        if dependency.extension_id == manifest.extension_id
            || !dependency_ids.insert(dependency.extension_id.as_str())
        {
            return Err(PluginManifestError::InvalidDependency);
        }
        validate_text(&dependency.version_requirement, limits.maximum_text_bytes)?;
    }
    Ok(())
}

fn validate_text(value: &str, maximum: usize) -> Result<(), PluginManifestError> {
    if value.is_empty() {
        return Err(PluginManifestError::InvalidText);
    }
    validate_text_allow_empty(value, maximum)
}

fn validate_text_allow_empty(value: &str, maximum: usize) -> Result<(), PluginManifestError> {
    if value.len() > maximum || value.contains('\0') {
        Err(PluginManifestError::InvalidText)
    } else {
        Ok(())
    }
}

fn validate_sha256(value: &str) -> Result<(), PluginManifestError> {
    let Some(digest) = value.strip_prefix(SHA256_PREFIX) else {
        return Err(PluginManifestError::InvalidContentHash);
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(PluginManifestError::InvalidContentHash);
    }
    Ok(())
}

fn validate_json(value: &Value, maximum: usize) -> Result<(), PluginManifestError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > maximum || json_depth(value) > 64 {
        Err(PluginManifestError::SchemaSize)
    } else {
        Ok(())
    }
}

fn json_depth(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1 + values.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(values) => 1 + values.values().map(json_depth).max().unwrap_or(0),
        _ => 1,
    }
}

fn contribution_id_set<'a>(values: impl Iterator<Item = &'a StableId>) -> BTreeSet<&'a str> {
    values.map(StableId::as_str).collect()
}

#[derive(Debug, Error)]
pub enum PluginManifestError {
    #[error("plugin manifest JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("plugin manifest limits are invalid")]
    InvalidLimits,
    #[error("plugin manifest is empty or exceeds its byte bound")]
    ManifestSize,
    #[error("unsupported plugin manifest schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("plugin manifest contains an empty, oversized, or NUL-containing text field")]
    InvalidText,
    #[error("plugin manifest content hash is not canonical sha256")]
    InvalidContentHash,
    #[error("plugin protocol version must be nonzero")]
    InvalidProtocolVersion,
    #[error("plugin entry point is malformed or exceeds its argv bound")]
    InvalidEntryPoint,
    #[error("plugin contribution count is outside its bound")]
    ContributionCount,
    #[error("plugin manifest repeats a contribution identity")]
    DuplicateContribution,
    #[error("plugin dependency count exceeds its bound")]
    DependencyCount,
    #[error("plugin dependency is duplicated or self-referential")]
    InvalidDependency,
    #[error("plugin schema or opaque contribution evidence exceeds its bound")]
    SchemaSize,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PluginPinError {
    #[error("plugin is disabled")]
    Disabled,
    #[error("plugin is incompatible with this Aworkit build")]
    Incompatible,
    #[error("plugin pin belongs to a different host generation")]
    HostGenerationDrift,
    #[error("plugin identity differs from the core pin")]
    IdentityDrift,
    #[error("plugin version differs from the core pin")]
    VersionDrift,
    #[error("plugin content hash differs from the core pin")]
    ContentHashDrift,
    #[error("plugin protocol version differs from the core pin")]
    ProtocolDrift,
    #[error("plugin contribution set differs from the core pin")]
    ContributionDrift,
}
