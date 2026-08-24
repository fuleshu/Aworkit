//! Generation-frozen capability descriptors used by invocation admission.

use std::collections::BTreeMap;

use aworkit_protocol::{
    AttestedExtensionSetV1, CapabilityDescriptorV1, CapabilityKindV1, CapabilitySideEffectV1,
    CapabilityVisibilityV1, EXTENSION_HOST_PROTOCOL_V1, ExtensionContributionV1,
    ExtensionIdentityV1, ExtensionRuntimeBindingV1, HostExtensionHandshakeV1, ProcessGeneration,
    StableId, attested_extension_set_hash_v1, capability_descriptor_hash_v1,
    host_extension_handshake_hash_v1, is_canonical_sha256,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable classes understood by the built-in host runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    Shell,
    Python,
    Model,
    FileRead,
    FileSearch,
    FileList,
    FileGrep,
    FileEdit,
    FileWrite,
    WebSearch,
    WebFetch,
    Todo,
    Subagent,
    Mcp,
    Plugin,
    ExternalAgent,
    Isolation,
}

/// Conservative side-effect semantics declared by an adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectClass {
    Pure,
    ReadOnly,
    IdempotentWrite,
    NonIdempotent,
    Unknown,
}

/// Compatibility descriptor kept for callers created by the early scaffold.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterDescriptor {
    pub capability_id: String,
    pub version: String,
    pub kind: CapabilityKind,
}

/// Complete immutable descriptor used for a production admission decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityDescriptor {
    pub capability_id: String,
    pub version: String,
    pub kind: CapabilityKind,
    pub side_effect: SideEffectClass,
    pub guarantees_same_id_deduplication: bool,
    pub supports_streaming: bool,
    pub supports_cancellation: bool,
    #[serde(default)]
    pub supports_continuation: bool,
    #[serde(default)]
    pub supports_sessions: bool,
    #[serde(default)]
    pub supports_approval_forwarding: bool,
    #[serde(default)]
    pub supports_mcp_forwarding: bool,
    pub allowed_scopes: Vec<String>,
    pub secret_slots: Vec<String>,
    #[serde(default)]
    pub input_schema_hash: Option<String>,
    #[serde(default)]
    pub output_schema_hash: Option<String>,
    #[serde(default)]
    pub requires_workspace: bool,
    #[serde(default)]
    pub required_isolation: Option<String>,
    #[serde(default = "default_maximum_concurrency")]
    pub maximum_concurrency: u32,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    #[serde(default)]
    pub supported_platforms: Vec<String>,
    #[serde(default = "default_visibility")]
    pub visibility: CapabilityVisibilityV1,
    pub version_hash: String,
}

impl CapabilityDescriptor {
    /// Builds and hashes a descriptor after normalizing set-like fields.
    pub fn build(
        capability_id: impl Into<String>,
        version: impl Into<String>,
        kind: CapabilityKind,
        side_effect: SideEffectClass,
    ) -> Result<Self, RegistryError> {
        let mut descriptor = Self {
            capability_id: capability_id.into(),
            version: version.into(),
            kind,
            side_effect,
            guarantees_same_id_deduplication: false,
            supports_streaming: false,
            supports_cancellation: false,
            supports_continuation: false,
            supports_sessions: false,
            supports_approval_forwarding: false,
            supports_mcp_forwarding: false,
            allowed_scopes: Vec::new(),
            secret_slots: Vec::new(),
            input_schema_hash: None,
            output_schema_hash: None,
            requires_workspace: false,
            required_isolation: None,
            maximum_concurrency: default_maximum_concurrency(),
            max_input_bytes: 1024 * 1024,
            max_output_bytes: 256 * 1024,
            supported_platforms: Vec::new(),
            visibility: default_visibility(),
            version_hash: String::new(),
        };
        descriptor.rehash()?;
        Ok(descriptor)
    }

    /// Recomputes the descriptor identity after a caller finishes configuration.
    pub fn rehash(&mut self) -> Result<(), RegistryError> {
        validate_name(&self.capability_id)?;
        validate_name(&self.version)?;
        if self.max_input_bytes == 0 || self.max_output_bytes == 0 {
            return Err(RegistryError::InvalidDescriptor);
        }
        self.allowed_scopes.sort();
        self.allowed_scopes.dedup();
        self.secret_slots.sort();
        self.secret_slots.dedup();
        self.supported_platforms.sort();
        self.supported_platforms.dedup();
        let wire = self.to_protocol_descriptor()?;
        self.version_hash =
            capability_descriptor_hash_v1(&wire).map_err(|_| RegistryError::InvalidDescriptor)?;
        Ok(())
    }

    fn to_protocol_descriptor(&self) -> Result<CapabilityDescriptorV1, RegistryError> {
        Ok(CapabilityDescriptorV1 {
            capability_id: StableId::parse(self.capability_id.clone())
                .map_err(|_| RegistryError::InvalidDescriptor)?,
            adapter_version: self.version.clone(),
            kind: protocol_kind(self.kind),
            side_effect: protocol_side_effect(self.side_effect),
            guarantees_same_id_deduplication: self.guarantees_same_id_deduplication,
            supports_streaming: self.supports_streaming,
            supports_cancellation: self.supports_cancellation,
            supports_continuation: self.supports_continuation,
            supports_sessions: self.supports_sessions,
            supports_approval_forwarding: self.supports_approval_forwarding,
            supports_mcp_forwarding: self.supports_mcp_forwarding,
            allowed_scopes: self.allowed_scopes.clone(),
            secret_slots: self.secret_slots.clone(),
            input_schema_hash: self.input_schema_hash.clone(),
            output_schema_hash: self.output_schema_hash.clone(),
            requires_workspace: self.requires_workspace,
            required_isolation: self.required_isolation.clone(),
            maximum_concurrency: self.maximum_concurrency,
            max_input_bytes: u64::try_from(self.max_input_bytes)
                .map_err(|_| RegistryError::InvalidDescriptor)?,
            max_output_bytes: u64::try_from(self.max_output_bytes)
                .map_err(|_| RegistryError::InvalidDescriptor)?,
            supported_platforms: self.supported_platforms.clone(),
            visibility: self.visibility,
            descriptor_hash: self.version_hash.clone(),
        })
    }
}

impl TryFrom<CapabilityDescriptorV1> for CapabilityDescriptor {
    type Error = RegistryError;

    fn try_from(value: CapabilityDescriptorV1) -> Result<Self, Self::Error> {
        if capability_descriptor_hash_v1(&value).map_err(|_| RegistryError::InvalidDescriptor)?
            != value.descriptor_hash
        {
            return Err(RegistryError::HashDrift);
        }
        let mut descriptor = Self {
            capability_id: value.capability_id.to_string(),
            version: value.adapter_version,
            kind: host_kind(value.kind),
            side_effect: host_side_effect(value.side_effect),
            guarantees_same_id_deduplication: value.guarantees_same_id_deduplication,
            supports_streaming: value.supports_streaming,
            supports_cancellation: value.supports_cancellation,
            supports_continuation: value.supports_continuation,
            supports_sessions: value.supports_sessions,
            supports_approval_forwarding: value.supports_approval_forwarding,
            supports_mcp_forwarding: value.supports_mcp_forwarding,
            allowed_scopes: value.allowed_scopes,
            secret_slots: value.secret_slots,
            input_schema_hash: value.input_schema_hash,
            output_schema_hash: value.output_schema_hash,
            requires_workspace: value.requires_workspace,
            required_isolation: value.required_isolation,
            maximum_concurrency: value.maximum_concurrency,
            max_input_bytes: usize::try_from(value.max_input_bytes)
                .map_err(|_| RegistryError::InvalidDescriptor)?,
            max_output_bytes: usize::try_from(value.max_output_bytes)
                .map_err(|_| RegistryError::InvalidDescriptor)?,
            supported_platforms: value.supported_platforms,
            visibility: value.visibility,
            version_hash: value.descriptor_hash,
        };
        let expected = descriptor.version_hash.clone();
        descriptor.rehash()?;
        if descriptor.version_hash != expected {
            return Err(RegistryError::HashDrift);
        }
        Ok(descriptor)
    }
}

impl TryFrom<AdapterDescriptor> for CapabilityDescriptor {
    type Error = RegistryError;

    fn try_from(value: AdapterDescriptor) -> Result<Self, Self::Error> {
        Self::build(
            value.capability_id,
            value.version,
            value.kind,
            SideEffectClass::Unknown,
        )
    }
}

/// Mutable registry used only while a host generation is being assembled.
#[derive(Clone, Default)]
pub struct AdapterRegistry {
    descriptors: BTreeMap<String, CapabilityDescriptor>,
}

impl AdapterRegistry {
    pub fn register(&mut self, descriptor: AdapterDescriptor) -> Result<(), RegistryError> {
        self.register_capability(descriptor.try_into()?)
    }

    pub fn register_capability(
        &mut self,
        mut descriptor: CapabilityDescriptor,
    ) -> Result<(), RegistryError> {
        descriptor.rehash()?;
        if self.descriptors.contains_key(&descriptor.capability_id) {
            return Err(RegistryError::Duplicate);
        }
        self.descriptors
            .insert(descriptor.capability_id.clone(), descriptor);
        Ok(())
    }

    pub fn resolve(&self, id: &str, version: &str) -> Result<&CapabilityDescriptor, RegistryError> {
        let value = self.descriptors.get(id).ok_or(RegistryError::Unknown)?;
        if value.version != version {
            return Err(RegistryError::VersionDrift);
        }
        Ok(value)
    }

    /// Consumes built-in descriptors and materializes only the exact extension
    /// contributions approved for the set's authenticated host generation.
    pub fn materialize_attested_set(
        mut self,
        set: &AttestedExtensionSetV1,
    ) -> Result<FrozenAdapterRegistry, RegistryError> {
        if set.host_generation.0 == 0 {
            return Err(RegistryError::InvalidAttestedSet);
        }
        if set.host_protocol != EXTENSION_HOST_PROTOCOL_V1 {
            return Err(RegistryError::UnsupportedAttestationProtocol);
        }
        if !is_canonical_sha256(&set.set_hash)
            || attested_extension_set_hash_v1(set).map_err(|_| RegistryError::InvalidAttestedSet)?
                != set.set_hash
        {
            return Err(RegistryError::AttestedSetHashDrift);
        }
        let mut extensions = set.extensions.clone();
        extensions.sort_by(|left, right| left.identity.cmp(&right.identity));
        if extensions
            .windows(2)
            .any(|pair| pair[0].identity == pair[1].identity)
        {
            return Err(RegistryError::Duplicate);
        }
        let mut contribution_ids = std::collections::BTreeSet::new();
        let mut extension_bindings = BTreeMap::new();
        for extension in extensions {
            if !is_canonical_sha256(&extension.identity.content_hash)
                || !is_canonical_sha256(&extension.handshake_hash)
            {
                return Err(RegistryError::InvalidAttestedSet);
            }
            for contribution in extension.contributions {
                if !contribution_ids.insert(contribution.contribution_id.clone()) {
                    return Err(RegistryError::Duplicate);
                }
                let capability_id = contribution.descriptor.capability_id.to_string();
                let binding = ExtensionRuntimeBindingV1 {
                    identity: extension.identity.clone(),
                    contribution_id: contribution.contribution_id,
                    host_id: set.host_id.clone(),
                    host_generation: set.host_generation,
                    handshake_hash: extension.handshake_hash.clone(),
                };
                self.register_capability(contribution.descriptor.try_into()?)?;
                if extension_bindings.insert(capability_id, binding).is_some() {
                    return Err(RegistryError::Duplicate);
                }
            }
        }
        Ok(FrozenAdapterRegistry {
            generation: set.host_generation,
            descriptors: self.descriptors,
            extension_bindings,
            attested_set_hash: Some(set.set_hash.clone()),
        })
    }

    /// Consumes the builder so an active generation cannot be changed in place.
    #[must_use]
    pub fn freeze(self, generation: ProcessGeneration) -> FrozenAdapterRegistry {
        FrozenAdapterRegistry {
            generation,
            descriptors: self.descriptors,
            extension_bindings: BTreeMap::new(),
            attested_set_hash: None,
        }
    }
}

/// Immutable exact descriptor set for one authenticated host generation.
#[derive(Clone)]
pub struct FrozenAdapterRegistry {
    generation: ProcessGeneration,
    descriptors: BTreeMap<String, CapabilityDescriptor>,
    extension_bindings: BTreeMap<String, ExtensionRuntimeBindingV1>,
    attested_set_hash: Option<String>,
}

impl FrozenAdapterRegistry {
    #[must_use]
    pub fn generation(&self) -> ProcessGeneration {
        self.generation
    }

    /// Returns the core-approved set identity when extensions were materialized.
    #[must_use]
    pub fn attested_set_hash(&self) -> Option<&str> {
        self.attested_set_hash.as_deref()
    }

    pub fn resolve_exact(
        &self,
        id: &str,
        version: &str,
        version_hash: &str,
    ) -> Result<&CapabilityDescriptor, RegistryError> {
        let descriptor = self.descriptors.get(id).ok_or(RegistryError::Unknown)?;
        if descriptor.version != version {
            return Err(RegistryError::VersionDrift);
        }
        if descriptor.version_hash != version_hash {
            return Err(RegistryError::HashDrift);
        }
        Ok(descriptor)
    }

    /// Resolves the descriptor and all executable provenance carried by a
    /// signed approved envelope. Extension-backed descriptors require the
    /// exact core-attested identity and handshake; built-ins require none.
    pub(crate) fn resolve_for_admission(
        &self,
        id: &str,
        version: &str,
        version_hash: &str,
        extension: Option<&ExtensionRuntimeBindingV1>,
        required_isolation_profile: Option<&str>,
    ) -> Result<&CapabilityDescriptor, RegistryError> {
        let descriptor = self.resolve_exact(id, version, version_hash)?;
        if self.extension_bindings.get(id) != extension {
            return Err(RegistryError::ExtensionAttestationDrift);
        }
        if descriptor.required_isolation.as_deref() != required_isolation_profile {
            return Err(RegistryError::IsolationProfileDrift);
        }
        Ok(descriptor)
    }
}

fn validate_name(value: &str) -> Result<(), RegistryError> {
    let valid = !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err(RegistryError::InvalidDescriptor)
    }
}

/// Builds a deterministic metadata-only handshake for core validation.
pub fn build_extension_handshake_v1(
    host_id: StableId,
    host_generation: ProcessGeneration,
    identity: ExtensionIdentityV1,
    entry_point_identity: String,
    healthy: bool,
    mut contributions: Vec<ExtensionContributionV1>,
) -> Result<HostExtensionHandshakeV1, RegistryError> {
    if host_generation.0 == 0 || entry_point_identity.is_empty() {
        return Err(RegistryError::InvalidAttestedSet);
    }
    contributions.sort_by(|left, right| {
        left.contribution_id
            .cmp(&right.contribution_id)
            .then_with(|| {
                left.descriptor
                    .capability_id
                    .cmp(&right.descriptor.capability_id)
            })
    });
    if contributions
        .windows(2)
        .any(|pair| pair[0].contribution_id == pair[1].contribution_id)
    {
        return Err(RegistryError::Duplicate);
    }
    for contribution in &contributions {
        CapabilityDescriptor::try_from(contribution.descriptor.clone())?;
    }
    let mut handshake = HostExtensionHandshakeV1 {
        host_id,
        host_generation,
        host_protocol: EXTENSION_HOST_PROTOCOL_V1,
        identity,
        entry_point_identity,
        healthy,
        contributions,
        handshake_hash: String::new(),
    };
    handshake.handshake_hash = host_extension_handshake_hash_v1(&handshake)
        .map_err(|_| RegistryError::InvalidAttestedSet)?;
    Ok(handshake)
}

fn protocol_kind(kind: CapabilityKind) -> CapabilityKindV1 {
    match kind {
        CapabilityKind::Shell => CapabilityKindV1::Shell,
        CapabilityKind::Python => CapabilityKindV1::Python,
        CapabilityKind::Model => CapabilityKindV1::Model,
        CapabilityKind::FileRead => CapabilityKindV1::FileRead,
        CapabilityKind::FileSearch => CapabilityKindV1::FileSearch,
        CapabilityKind::FileList => CapabilityKindV1::FileList,
        CapabilityKind::FileGrep => CapabilityKindV1::FileGrep,
        CapabilityKind::FileEdit => CapabilityKindV1::FileEdit,
        CapabilityKind::FileWrite => CapabilityKindV1::FileWrite,
        CapabilityKind::WebSearch => CapabilityKindV1::WebSearch,
        CapabilityKind::WebFetch => CapabilityKindV1::WebFetch,
        CapabilityKind::Todo => CapabilityKindV1::Todo,
        CapabilityKind::Subagent => CapabilityKindV1::Subagent,
        CapabilityKind::Mcp => CapabilityKindV1::Mcp,
        CapabilityKind::Plugin => CapabilityKindV1::Plugin,
        CapabilityKind::ExternalAgent => CapabilityKindV1::ExternalAgent,
        CapabilityKind::Isolation => CapabilityKindV1::Isolation,
    }
}

fn host_kind(kind: CapabilityKindV1) -> CapabilityKind {
    match kind {
        CapabilityKindV1::Shell => CapabilityKind::Shell,
        CapabilityKindV1::Python => CapabilityKind::Python,
        CapabilityKindV1::Model => CapabilityKind::Model,
        CapabilityKindV1::FileRead => CapabilityKind::FileRead,
        CapabilityKindV1::FileSearch => CapabilityKind::FileSearch,
        CapabilityKindV1::FileList => CapabilityKind::FileList,
        CapabilityKindV1::FileGrep => CapabilityKind::FileGrep,
        CapabilityKindV1::FileEdit => CapabilityKind::FileEdit,
        CapabilityKindV1::FileWrite => CapabilityKind::FileWrite,
        CapabilityKindV1::WebSearch => CapabilityKind::WebSearch,
        CapabilityKindV1::WebFetch => CapabilityKind::WebFetch,
        CapabilityKindV1::Todo => CapabilityKind::Todo,
        CapabilityKindV1::Subagent => CapabilityKind::Subagent,
        CapabilityKindV1::Mcp => CapabilityKind::Mcp,
        CapabilityKindV1::Plugin => CapabilityKind::Plugin,
        CapabilityKindV1::ExternalAgent => CapabilityKind::ExternalAgent,
        CapabilityKindV1::Isolation => CapabilityKind::Isolation,
    }
}

fn protocol_side_effect(side_effect: SideEffectClass) -> CapabilitySideEffectV1 {
    match side_effect {
        SideEffectClass::Pure => CapabilitySideEffectV1::Pure,
        SideEffectClass::ReadOnly => CapabilitySideEffectV1::ReadOnly,
        SideEffectClass::IdempotentWrite => CapabilitySideEffectV1::IdempotentWrite,
        SideEffectClass::NonIdempotent => CapabilitySideEffectV1::NonIdempotent,
        SideEffectClass::Unknown => CapabilitySideEffectV1::Unknown,
    }
}

fn host_side_effect(side_effect: CapabilitySideEffectV1) -> SideEffectClass {
    match side_effect {
        CapabilitySideEffectV1::Pure => SideEffectClass::Pure,
        CapabilitySideEffectV1::ReadOnly => SideEffectClass::ReadOnly,
        CapabilitySideEffectV1::IdempotentWrite => SideEffectClass::IdempotentWrite,
        CapabilitySideEffectV1::NonIdempotent => SideEffectClass::NonIdempotent,
        CapabilitySideEffectV1::Unknown => SideEffectClass::Unknown,
    }
}

const fn default_maximum_concurrency() -> u32 {
    1
}

const fn default_visibility() -> CapabilityVisibilityV1 {
    CapabilityVisibilityV1::Mediated
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RegistryError {
    #[error("duplicate adapter descriptor")]
    Duplicate,
    #[error("unknown approved adapter")]
    Unknown,
    #[error("adapter version drift")]
    VersionDrift,
    #[error("adapter descriptor hash drift")]
    HashDrift,
    #[error("adapter extension identity or host attestation drift")]
    ExtensionAttestationDrift,
    #[error("adapter required isolation profile drift")]
    IsolationProfileDrift,
    #[error("adapter descriptor is malformed")]
    InvalidDescriptor,
    #[error("attested extension set is malformed")]
    InvalidAttestedSet,
    #[error("attested extension set hash drifted")]
    AttestedSetHashDrift,
    #[error("attested extension set uses an unsupported protocol")]
    UnsupportedAttestationProtocol,
}
