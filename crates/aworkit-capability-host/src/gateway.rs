use aworkit_protocol::{ProcessGeneration, StableId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use crate::{AdapterRegistry, CapabilityKind, registry::RegistryError};
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovedInvocation { pub invocation_id: StableId, pub host_generation: ProcessGeneration, pub capability_id: String, pub adapter_version: String, pub kind: CapabilityKind, pub payload: Value }
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InvocationResult { pub invocation_id: StableId, pub succeeded: bool, pub side_effect_known_safe: bool, pub payload: Value }
pub struct CapabilityHost { generation: ProcessGeneration, registry: AdapterRegistry }
impl CapabilityHost { #[must_use] pub fn new(generation: ProcessGeneration, registry: AdapterRegistry) -> Self { Self { generation, registry } } pub fn admit(&self, envelope: &ApprovedInvocation) -> Result<(), HostError> { if envelope.host_generation != self.generation { return Err(HostError::StaleGeneration); } let descriptor = self.registry.resolve(&envelope.capability_id, &envelope.adapter_version)?; if descriptor.kind != envelope.kind { return Err(HostError::KindMismatch); } Ok(()) } }
#[derive(Debug, Error)]
pub enum HostError { #[error("stale host generation")] StaleGeneration, #[error("adapter kind differs from its frozen descriptor")] KindMismatch, #[error(transparent)] Registry(#[from] RegistryError) }
