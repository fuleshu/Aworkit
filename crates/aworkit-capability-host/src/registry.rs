//! Generation-frozen capability descriptors used by invocation admission.

use std::collections::BTreeMap;

use aworkit_protocol::ProcessGeneration;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
    FileEdit,
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
    pub allowed_scopes: Vec<String>,
    pub secret_slots: Vec<String>,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
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
            allowed_scopes: Vec::new(),
            secret_slots: Vec::new(),
            max_input_bytes: 1024 * 1024,
            max_output_bytes: 256 * 1024,
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
        let hash_input = (
            &self.capability_id,
            &self.version,
            self.kind,
            self.side_effect,
            self.guarantees_same_id_deduplication,
            self.supports_streaming,
            self.supports_cancellation,
            &self.allowed_scopes,
            &self.secret_slots,
            self.max_input_bytes,
            self.max_output_bytes,
        );
        let bytes =
            serde_json::to_vec(&hash_input).map_err(|_| RegistryError::InvalidDescriptor)?;
        self.version_hash = format!("sha256:{:x}", Sha256::digest(bytes));
        Ok(())
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

    /// Consumes the builder so an active generation cannot be changed in place.
    #[must_use]
    pub fn freeze(self, generation: ProcessGeneration) -> FrozenAdapterRegistry {
        FrozenAdapterRegistry {
            generation,
            descriptors: self.descriptors,
        }
    }
}

/// Immutable exact descriptor set for one authenticated host generation.
#[derive(Clone)]
pub struct FrozenAdapterRegistry {
    generation: ProcessGeneration,
    descriptors: BTreeMap<String, CapabilityDescriptor>,
}

impl FrozenAdapterRegistry {
    #[must_use]
    pub fn generation(&self) -> ProcessGeneration {
        self.generation
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

    pub(crate) fn resolve_version(
        &self,
        id: &str,
        version: &str,
    ) -> Result<&CapabilityDescriptor, RegistryError> {
        let descriptor = self.descriptors.get(id).ok_or(RegistryError::Unknown)?;
        if descriptor.version != version {
            return Err(RegistryError::VersionDrift);
        }
        Ok(descriptor)
    }
}

fn validate_name(value: &str) -> Result<(), RegistryError> {
    let valid = !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'/'));
    if valid {
        Ok(())
    } else {
        Err(RegistryError::InvalidDescriptor)
    }
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
    #[error("adapter descriptor is malformed")]
    InvalidDescriptor,
}
